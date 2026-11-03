//! Target-independent encoder decisions: which picture comes next, how a
//! bitrate maps onto libva's rate control buffers, and how input frames are
//! padded to whole macroblocks.

use super::{
    VaapiError,
    headers::{FrameKind, LOG2_MAX_FRAME_NUM, LOG2_MAX_POC_LSB},
    types::{
        VA_RC_CBR, VA_RC_VBR, VAEncMiscParameterFrameRate, VAEncMiscParameterHRD,
        VAEncMiscParameterRateControl,
    },
};
use crate::{Bitrate, EncoderProfile, FrameRate};
use std::fmt;

/// Pictures between POC wraps of the full counter; the POC of picture `n`
/// is `2n`, so the 16-bit counter wraps after this many.
const POC_WRAP_PICTURES: u64 = 1 << 15;

/// The coding decisions for one picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramePlan {
    /// IDR or P.
    pub kind: FrameKind,
    /// `frame_num`.
    pub frame_num: u32,
    /// The picture order count, two per picture.
    pub poc: u32,
    /// `idr_pic_id`.
    pub idr_pic_id: u32,
}

impl FramePlan {
    /// Returns `pic_order_cnt_lsb`.
    pub const fn poc_lsb(&self) -> u32 {
        self.poc % (1 << LOG2_MAX_POC_LSB)
    }
}

/// Decides the picture type, `frame_num` and POC of each submitted frame.
#[derive(Debug, Clone, Copy)]
pub struct Sequencer {
    idr_period: Option<u64>,
    /// The position of the last coded picture in its coded video sequence,
    /// or `None` when the next picture must be an IDR.
    last_index: Option<u64>,
    next_idr_pic_id: u32,
}

impl Sequencer {
    /// Creates a sequencer that forces an IDR every `idr_period` pictures,
    /// or only on request when it is `None`.
    pub const fn new(idr_period: Option<u64>) -> Self {
        Self {
            idr_period,
            last_index: None,
            next_idr_pic_id: 0,
        }
    }

    /// Makes the next picture an IDR, as after a failed frame whose
    /// reference is unknown.
    pub const fn restart(&mut self) {
        self.last_index = None;
    }

    /// Plans the next picture; `keyframe` forces an IDR.
    pub fn plan(&mut self, keyframe: bool) -> FramePlan {
        let next = match self.last_index {
            Some(last) if !keyframe && self.idr_period.is_none_or(|period| last + 1 < period) => {
                Some(last + 1)
            }
            _ => None,
        };
        let (kind, index, idr_pic_id) = if let Some(index) = next {
            (FrameKind::P, index, 0)
        } else {
            let idr_pic_id = self.next_idr_pic_id;
            // `idr_pic_id` ranges over 0..=65535 and consecutive IDRs differ.
            self.next_idr_pic_id = (idr_pic_id + 1) % (1 << 16);
            (FrameKind::Idr, 0, idr_pic_id)
        };
        self.last_index = Some(index);
        FramePlan {
            kind,
            frame_num: u32::try_from(index % (1 << LOG2_MAX_FRAME_NUM)).unwrap_or_default(),
            poc: u32::try_from((index % POC_WRAP_PICTURES) * 2).unwrap_or_default(),
            idr_pic_id,
        }
    }
}

/// The rate control modes the encoder drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateControlMode {
    /// Constant bitrate, for live streams.
    Cbr,
    /// Variable bitrate around the target, for offline work.
    Vbr,
}

impl RateControlMode {
    /// Returns the mode an encoder profile asks for.
    pub const fn for_profile(profile: EncoderProfile) -> Self {
        match profile {
            EncoderProfile::Realtime => Self::Cbr,
            EncoderProfile::Offline => Self::Vbr,
        }
    }

    /// Returns the `VA_RC_*` bit of `VAConfigAttribRateControl`.
    pub const fn attribute_bit(self) -> u32 {
        match self {
            Self::Cbr => VA_RC_CBR,
            Self::Vbr => VA_RC_VBR,
        }
    }
}

impl fmt::Display for RateControlMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Cbr => "CBR",
            Self::Vbr => "VBR",
        })
    }
}

/// The rate control window of a live stream, which is also its decoder
/// buffer: half a second of the target rate.
const REALTIME_WINDOW_MS: u32 = 500;
/// The rate control window of an offline stream.
const OFFLINE_WINDOW_MS: u32 = 1_500;
/// A VBR stream may peak at `PEAK_NUMERATOR / PEAK_DENOMINATOR` of its
/// target; libva expresses the target as a percentage of the peak.
const PEAK_NUMERATOR: u64 = 5;
const PEAK_DENOMINATOR: u64 = 4;
/// The target as a percentage of that peak, `100 * PEAK_DENOMINATOR / PEAK_NUMERATOR`.
const VBR_TARGET_PERCENTAGE: u32 = 80;

/// libva's rate control parameters for one target bitrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateControl {
    /// The mode.
    pub mode: RateControlMode,
    /// `bits_per_second`: the rate for CBR, the peak for VBR.
    pub bits_per_second: u32,
    /// `target_percentage`.
    pub target_percentage: u32,
    /// `window_size` in milliseconds.
    pub window_ms: u32,
    /// The decoder buffer size in bits.
    pub buffer_bits: u32,
    /// The decoder buffer fullness at the start, in bits.
    pub initial_buffer_bits: u32,
}

impl RateControl {
    /// Maps `bitrate` for an encoder of `profile`.
    ///
    /// # Errors
    ///
    /// Returns [`VaapiError::BitrateOutOfRange`] when the peak rate or the
    /// buffer does not fit 32 bits.
    pub fn new(profile: EncoderProfile, bitrate: Bitrate) -> Result<Self, VaapiError> {
        let target = u64::from(bitrate.bits_per_second().get());
        let mode = RateControlMode::for_profile(profile);
        let (window_ms, peak, target_percentage) = match mode {
            RateControlMode::Cbr => (REALTIME_WINDOW_MS, target, 100),
            RateControlMode::Vbr => (
                OFFLINE_WINDOW_MS,
                target * PEAK_NUMERATOR / PEAK_DENOMINATOR,
                VBR_TARGET_PERCENTAGE,
            ),
        };
        let buffer = peak * u64::from(window_ms) / 1_000;
        let narrow =
            |value: u64| u32::try_from(value).map_err(|_| VaapiError::BitrateOutOfRange(value));
        Ok(Self {
            mode,
            bits_per_second: narrow(peak)?,
            target_percentage,
            window_ms,
            buffer_bits: narrow(buffer)?,
            initial_buffer_bits: narrow(buffer / 4 * 3)?,
        })
    }

    /// Returns the `VAEncMiscParameterRateControl` payload.
    pub fn misc_rate_control(&self) -> VAEncMiscParameterRateControl {
        VAEncMiscParameterRateControl {
            bits_per_second: self.bits_per_second,
            target_percentage: self.target_percentage,
            window_size: self.window_ms,
            ..VAEncMiscParameterRateControl::default()
        }
    }

    /// Returns the `VAEncMiscParameterHRD` payload.
    pub fn misc_hrd(&self) -> VAEncMiscParameterHRD {
        VAEncMiscParameterHRD {
            initial_buffer_fullness: self.initial_buffer_bits,
            buffer_size: self.buffer_bits,
            ..VAEncMiscParameterHRD::default()
        }
    }
}

/// Returns the `VAEncMiscParameterFrameRate` payload for `frame_rate`.
///
/// libva packs the ratio into two 16-bit halves, so the ratio is reduced
/// first.
///
/// # Errors
///
/// Returns [`VaapiError::FrameRateOutOfRange`] when the reduced ratio does
/// not fit.
pub fn misc_frame_rate(frame_rate: FrameRate) -> Result<VAEncMiscParameterFrameRate, VaapiError> {
    let divisor = gcd(frame_rate.numerator().get(), frame_rate.denominator().get());
    let numerator = u16::try_from(frame_rate.numerator().get() / divisor)
        .map_err(|_| VaapiError::FrameRateOutOfRange(frame_rate))?;
    let denominator = u16::try_from(frame_rate.denominator().get() / divisor)
        .map_err(|_| VaapiError::FrameRateOutOfRange(frame_rate))?;
    Ok(VAEncMiscParameterFrameRate {
        framerate: (u32::from(denominator) << 16) | u32::from(numerator),
        ..VAEncMiscParameterFrameRate::default()
    })
}

const fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Where the planes of a mapped NV12 `VAImage` lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nv12Planes {
    /// Bytes from one luma row to the next.
    pub y_pitch: usize,
    /// Offset of the luma plane.
    pub y_offset: usize,
    /// Bytes from one chroma row to the next.
    pub uv_pitch: usize,
    /// Offset of the interleaved chroma plane.
    pub uv_offset: usize,
}

/// Copies a tightly packed NV12 frame of `width` x `height` into `image`
/// laid out as `planes`, `coded_width` x `coded_height` samples large.
///
/// The area beyond the frame, up to the macroblock-aligned size, repeats the
/// last column and row, which keeps the padding cheap to code and the edge
/// free of artefacts.
///
/// # Errors
///
/// Returns [`VaapiError::BufferTooSmall`] when `frame` or `image` cannot
/// hold the layout.
pub fn copy_nv12_padded(
    frame: &[u8],
    (width, height): (usize, usize),
    image: &mut [u8],
    (coded_width, coded_height): (usize, usize),
    planes: Nv12Planes,
) -> Result<(), VaapiError> {
    let luma = width * height;
    let (frame_luma, frame_chroma) = frame
        .split_at_checked(luma)
        .ok_or(VaapiError::BufferTooSmall("input frame"))?;
    pad_plane(
        frame_luma,
        (width, height),
        1,
        image,
        (coded_width, coded_height),
        (planes.y_offset, planes.y_pitch),
    )?;
    // Chroma is half height, with one interleaved Cb/Cr pair per two columns.
    pad_plane(
        frame_chroma,
        (width, height / 2),
        2,
        image,
        (coded_width, coded_height / 2),
        (planes.uv_offset, planes.uv_pitch),
    )
}

/// Copies one plane of `unit`-byte samples and repeats its last sample and
/// row to fill `coded`.
fn pad_plane(
    source: &[u8],
    (width, height): (usize, usize),
    unit: usize,
    image: &mut [u8],
    (coded_width, coded_height): (usize, usize),
    (offset, pitch): (usize, usize),
) -> Result<(), VaapiError> {
    for row in 0..coded_height {
        let source_row = source
            .get(row.min(height.saturating_sub(1)) * width..)
            .and_then(|rest| rest.get(..width))
            .ok_or(VaapiError::BufferTooSmall("input frame"))?;
        let start = offset + row * pitch;
        let target = image
            .get_mut(start..start + coded_width)
            .ok_or(VaapiError::BufferTooSmall("image"))?;
        let (visible, padding) = target.split_at_mut(width);
        visible.copy_from_slice(source_row);
        // Repeat the last sample (one byte of luma, one Cb/Cr pair of chroma).
        let tail = source_row
            .get(width - unit..)
            .ok_or(VaapiError::BufferTooSmall("input frame"))?;
        for chunk in padding.chunks_mut(unit) {
            chunk.copy_from_slice(&tail[..chunk.len()]);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use FrameKind::{Idr, P};
    use std::num::NonZeroU32;

    fn bitrate(bits_per_second: u32) -> Bitrate {
        Bitrate::new(NonZeroU32::new(bits_per_second).unwrap())
    }

    fn frame_rate(numerator: u32, denominator: u32) -> FrameRate {
        FrameRate::new(
            NonZeroU32::new(numerator).unwrap(),
            NonZeroU32::new(denominator).unwrap(),
        )
    }

    #[test]
    fn the_first_picture_is_an_idr_and_the_rest_are_p_pictures() {
        let mut sequencer = Sequencer::new(None);
        let plans: Vec<_> = (0..3).map(|_| sequencer.plan(false)).collect();
        assert_eq!(plans[0].kind, FrameKind::Idr);
        assert_eq!(plans[1].kind, FrameKind::P);
        assert_eq!(
            plans.map_frame_nums(),
            [(0, 0, 0), (1, 2, 2), (2, 4, 4)],
            "frame_num, POC and POC lsb advance by one and two"
        );
    }

    trait FramePlans {
        fn map_frame_nums(&self) -> Vec<(u32, u32, u32)>;
    }

    impl FramePlans for Vec<FramePlan> {
        fn map_frame_nums(&self) -> Vec<(u32, u32, u32)> {
            self.iter()
                .map(|plan| (plan.frame_num, plan.poc, plan.poc_lsb()))
                .collect()
        }
    }

    #[test]
    fn a_keyframe_request_restarts_numbering_and_changes_idr_pic_id() {
        let mut sequencer = Sequencer::new(None);
        let first = sequencer.plan(false);
        sequencer.plan(false);
        let second = sequencer.plan(true);
        let next = sequencer.plan(false);
        assert_eq!(
            (second.kind, second.frame_num, second.poc),
            (FrameKind::Idr, 0, 0)
        );
        assert_ne!(first.idr_pic_id, second.idr_pic_id);
        assert_eq!((next.kind, next.frame_num, next.poc), (FrameKind::P, 1, 2));
    }

    #[test]
    fn the_idr_period_counts_pictures_per_sequence() {
        let mut sequencer = Sequencer::new(Some(3));
        let kinds: Vec<_> = (0..7).map(|_| sequencer.plan(false).kind).collect();
        assert_eq!(kinds, [Idr, P, P, Idr, P, P, Idr]);
        let mut every_picture = Sequencer::new(Some(1));
        assert!((0..4).all(|_| every_picture.plan(false).kind == Idr));
    }

    #[test]
    fn frame_num_and_poc_wrap_with_their_syntax_elements() {
        let mut sequencer = Sequencer::new(None);
        let plans: Vec<_> = (0..300).map(|_| sequencer.plan(false)).collect();
        assert_eq!(plans[255].frame_num, 255);
        assert_eq!(plans[256].frame_num, 0, "frame_num is 8 bits");
        assert_eq!(plans[256].kind, FrameKind::P);
        assert_eq!(
            plans[128].poc_lsb(),
            0,
            "POC lsb is 8 bits at two per picture"
        );
        assert_eq!(plans[127].poc_lsb(), 254);
        // The full POC wraps on a multiple of the lsb range, keeping the two
        // consistent.
        let mut sequencer = Sequencer::new(None);
        let mut last = sequencer.plan(false);
        for _ in 1..=40_000 {
            let plan = sequencer.plan(false);
            assert_eq!(plan.poc_lsb(), (last.poc_lsb() + 2) % 256);
            assert_eq!(plan.frame_num, (last.frame_num + 1) % 256);
            last = plan;
        }
    }

    #[test]
    fn idr_pic_id_wraps_at_sixteen_bits_and_a_restart_forces_an_idr() {
        let mut sequencer = Sequencer::new(Some(1));
        let ids: Vec<_> = (0..65_538)
            .map(|_| sequencer.plan(false).idr_pic_id)
            .collect();
        assert_eq!(ids[65_535], 65_535);
        assert_eq!(ids[65_536], 0);
        let mut sequencer = Sequencer::new(None);
        sequencer.plan(false);
        sequencer.restart();
        assert_eq!(sequencer.plan(false).kind, FrameKind::Idr);
    }

    #[test]
    fn realtime_is_cbr_and_offline_is_vbr_around_the_target() {
        let cbr = RateControl::new(EncoderProfile::Realtime, bitrate(4_000_000)).unwrap();
        assert_eq!(cbr.mode, RateControlMode::Cbr);
        assert_eq!(
            (cbr.bits_per_second, cbr.target_percentage, cbr.window_ms),
            (4_000_000, 100, 500)
        );
        assert_eq!(
            (cbr.buffer_bits, cbr.initial_buffer_bits),
            (2_000_000, 1_500_000)
        );
        let rate = cbr.misc_rate_control();
        assert_eq!(
            (rate.bits_per_second, rate.window_size, rate.rc_flags),
            (4_000_000, 500, 0)
        );
        let hrd = cbr.misc_hrd();
        assert_eq!(
            (hrd.buffer_size, hrd.initial_buffer_fullness),
            (2_000_000, 1_500_000)
        );

        let vbr = RateControl::new(EncoderProfile::Offline, bitrate(4_000_000)).unwrap();
        assert_eq!(vbr.mode, RateControlMode::Vbr);
        // A 5 Mbit/s peak of which 80 % is the 4 Mbit/s target.
        assert_eq!(
            (vbr.bits_per_second, vbr.target_percentage),
            (5_000_000, 80)
        );
        assert_eq!(vbr.bits_per_second / 100 * vbr.target_percentage, 4_000_000);
        assert_eq!(
            RateControlMode::for_profile(EncoderProfile::Offline),
            RateControlMode::Vbr
        );
        assert_eq!(RateControlMode::Cbr.to_string(), "CBR");
        assert_eq!(RateControlMode::Vbr.attribute_bit(), 4);
    }

    #[test]
    fn a_rate_that_overflows_libvas_fields_is_an_error() {
        assert!(matches!(
            RateControl::new(EncoderProfile::Offline, bitrate(u32::MAX)),
            Err(VaapiError::BitrateOutOfRange(_))
        ));
        assert!(RateControl::new(EncoderProfile::Realtime, bitrate(u32::MAX)).is_ok());
    }

    #[test]
    fn frame_rates_are_reduced_into_libvas_sixteen_bit_halves() {
        let ntsc = misc_frame_rate(frame_rate(30_000, 1001)).unwrap();
        assert_eq!(ntsc.framerate, (1001 << 16) | 0x7530);
        let reduced = misc_frame_rate(frame_rate(60_000, 2_000)).unwrap();
        assert_eq!(reduced.framerate, (1 << 16) | 0x1e);
        assert!(matches!(
            misc_frame_rate(frame_rate(65_536, 1)),
            Err(VaapiError::FrameRateOutOfRange(_))
        ));
        assert!(misc_frame_rate(frame_rate(65_535, 1)).is_ok());
    }

    fn layout() -> Nv12Planes {
        Nv12Planes {
            y_pitch: 12,
            y_offset: 0,
            uv_pitch: 12,
            uv_offset: 12 * 8,
        }
    }

    #[test]
    fn padding_repeats_the_last_column_and_row() {
        // 4x2 luma and 2x1 chroma pairs, padded to 8x8 samples.
        let frame: Vec<u8> = vec![1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 12, 13];
        let mut image = vec![0xEE; 12 * 12];
        copy_nv12_padded(&frame, (4, 2), &mut image, (8, 8), layout()).unwrap();
        assert_eq!(image[..8], [1, 2, 3, 4, 4, 4, 4, 4]);
        assert_eq!(image[12..20], [5, 6, 7, 8, 8, 8, 8, 8]);
        assert_eq!(image[7 * 12..7 * 12 + 8], [5, 6, 7, 8, 8, 8, 8, 8]);
        assert_eq!(
            image[8..12],
            [0xEE; 4],
            "bytes beyond the coded width are untouched"
        );
        let chroma = 12 * 8;
        assert_eq!(image[chroma..chroma + 8], [10, 11, 12, 13, 12, 13, 12, 13]);
        assert_eq!(
            image[chroma + 3 * 12..chroma + 3 * 12 + 8],
            [10, 11, 12, 13, 12, 13, 12, 13]
        );
    }

    #[test]
    fn padding_reports_buffers_that_are_too_small() {
        let frame = vec![0; 12];
        let mut image = vec![0; 12 * 12];
        assert!(matches!(
            copy_nv12_padded(&frame[..11], (4, 2), &mut image, (8, 8), layout()),
            Err(VaapiError::BufferTooSmall("input frame"))
        ));
        assert!(matches!(
            copy_nv12_padded(&frame, (4, 2), &mut image[..100], (8, 8), layout()),
            Err(VaapiError::BufferTooSmall("image"))
        ));
        assert!(matches!(
            copy_nv12_padded(
                &frame,
                (4, 2),
                &mut image,
                (8, 8),
                Nv12Planes {
                    uv_offset: 12 * 11,
                    ..layout()
                }
            ),
            Err(VaapiError::BufferTooSmall("image"))
        ));
    }
}
