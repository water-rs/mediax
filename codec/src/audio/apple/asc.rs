//! `AudioSpecificConfig` (ISO/IEC 14496-3) parsing: the output format an AAC
//! stream decodes to, including spectral band replication and parametric stereo.

const SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];
const CHANNEL_COUNTS: [u16; 7] = [1, 2, 3, 4, 5, 6, 8];
const OBJECT_TYPE_ESCAPE: u32 = 31;
const SAMPLE_RATE_ESCAPE: u32 = 15;
const OBJECT_SBR: u32 = 5;
const OBJECT_PS: u32 = 29;
const SYNC_EXTENSION_SBR: u32 = 0x2b7;
const SYNC_EXTENSION_PS: u32 = 0x548;

/// What an `AudioSpecificConfig` says about the decoded stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StreamFormat {
    /// Decoded sample rate: the SBR extension rate when SBR is signalled.
    pub(super) sample_rate: u32,
    /// Decoded channel count: two when parametric stereo is signalled.
    pub(super) channels: u16,
    pub(super) sbr: bool,
    pub(super) parametric_stereo: bool,
}

struct BitReader<'a> {
    data: &'a [u8],
    position: usize,
}

impl BitReader<'_> {
    const fn remaining(&self) -> usize {
        self.data.len() * 8 - self.position
    }

    fn read(&mut self, count: usize) -> Result<u32, String> {
        if self.remaining() < count {
            return Err(String::from("AudioSpecificConfig is truncated"));
        }
        let mut value = 0;
        for _ in 0..count {
            let byte = self.data[self.position / 8];
            value = (value << 1) | u32::from((byte >> (7 - self.position % 8)) & 1);
            self.position += 1;
        }
        Ok(value)
    }

    fn object_type(&mut self) -> Result<u32, String> {
        match self.read(5)? {
            OBJECT_TYPE_ESCAPE => Ok(32 + self.read(6)?),
            object_type => Ok(object_type),
        }
    }

    fn sample_rate(&mut self) -> Result<u32, String> {
        match self.read(4)? {
            SAMPLE_RATE_ESCAPE => self.read(24),
            index => SAMPLE_RATES
                .get(usize::try_from(index).expect("4-bit index fits usize"))
                .copied()
                .ok_or_else(|| format!("reserved AAC sampling frequency index {index}")),
        }
    }
}

impl StreamFormat {
    /// Parses the decoded format out of `AudioSpecificConfig` bytes.
    pub(super) fn parse(audio_specific_config: &[u8]) -> Result<Self, String> {
        let mut reader = BitReader {
            data: audio_specific_config,
            position: 0,
        };
        let mut object_type = reader.object_type()?;
        let core_rate = reader.sample_rate()?;
        let channel_config = reader.read(4)?;
        let mut sample_rate = core_rate;
        let mut sbr = false;
        let mut parametric_stereo = false;
        if object_type == OBJECT_SBR || object_type == OBJECT_PS {
            sbr = true;
            parametric_stereo = object_type == OBJECT_PS;
            sample_rate = reader.sample_rate()?;
            object_type = reader.object_type()?;
        }

        let channels = match channel_config {
            0 => {
                return Err(String::from(
                    "AudioSpecificConfig with a program config element is unsupported",
                ));
            }
            1..=7 => {
                CHANNEL_COUNTS[usize::try_from(channel_config - 1)
                    .expect("3-bit channel configuration fits usize")]
            }
            _ => {
                return Err(format!(
                    "reserved AAC channel configuration {channel_config}"
                ));
            }
        };

        // Backward-compatible signalling: the SBR/PS extension follows the
        // `GASpecificConfig` of a plain AAC-LC-family core.
        if !sbr && (1..=4).contains(&object_type) {
            // frameLengthFlag, dependsOnCoreCoder (+14 delay bits), extensionFlag.
            reader.read(1)?;
            if reader.read(1)? == 1 {
                reader.read(14)?;
            }
            reader.read(1)?;
            if reader.remaining() >= 16
                && reader.read(11)? == SYNC_EXTENSION_SBR
                && reader.read(5)? == OBJECT_SBR
                && reader.read(1)? == 1
            {
                sbr = true;
                sample_rate = reader.sample_rate()?;
                if reader.remaining() >= 12 && reader.read(11)? == SYNC_EXTENSION_PS {
                    parametric_stereo = reader.read(1)? == 1;
                }
            }
        }

        Ok(Self {
            sample_rate,
            channels: if parametric_stereo { 2 } else { channels },
            sbr,
            parametric_stereo,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::StreamFormat;

    #[test]
    fn parses_aac_lc() {
        assert_eq!(
            StreamFormat::parse(&[0x12, 0x10]),
            Ok(StreamFormat {
                sample_rate: 44_100,
                channels: 2,
                sbr: false,
                parametric_stereo: false,
            })
        );
    }

    #[test]
    fn parses_explicit_sbr() {
        // objectType 5, 24 kHz core, stereo, 48 kHz extension, objectType 2.
        let format = StreamFormat::parse(&[0x2b, 0x11, 0x88, 0x00]).expect("valid HE-AAC config");
        assert_eq!((format.sample_rate, format.channels), (48_000, 2));
        assert!(format.sbr && !format.parametric_stereo);
    }

    #[test]
    fn parses_explicit_parametric_stereo() {
        // objectType 29, 24 kHz core, mono, 48 kHz extension, objectType 2.
        let format = StreamFormat::parse(&[0xeb, 0x09, 0x88, 0x00]).expect("valid HE-AACv2 config");
        assert_eq!((format.sample_rate, format.channels), (48_000, 2));
        assert!(format.sbr && format.parametric_stereo);
    }

    #[test]
    fn rejects_truncated_config() {
        assert!(StreamFormat::parse(&[0x12]).is_err());
    }
}
