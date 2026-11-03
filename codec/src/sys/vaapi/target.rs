//! The surfaces a decoder renders into and reads frames back from, shared by
//! the H.264 and H.265 decoders.

use super::{
    VaapiError,
    device::{Config, Context, Device, Image, PictureBuffers, SampleFormat, Surfaces, to_usize},
    plan::Nv12Planes,
    types::{
        VA_ATTRIB_NOT_SUPPORTED, VA_INVALID_ID, VAConfigAttrib, VAConfigAttribRTFormat,
        VAEntrypointVLD, VASurfaceID,
    },
};
use crate::DecodedPixelLayout;
use std::{collections::HashMap, ffi::c_int, hash::Hash, rc::Rc};

/// One decoded frame: tightly packed semi-planar 4:2:0 of the cropped size.
#[derive(Debug)]
pub struct DecodedPicture {
    /// The planes, luma first, then interleaved chroma. Samples of 10-bit
    /// pictures are 16-bit little-endian, in the high bits.
    pub data: Vec<u8>,
    /// The frame width in samples.
    pub width: u32,
    /// The frame height in samples.
    pub height: u32,
    /// The timestamp of the packet the picture came in.
    pub timestamp_ns: u64,
    /// How the samples are laid out.
    pub layout: DecodedPixelLayout,
}

/// The visible rectangle of the decoded surface, in luma samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Visible {
    /// Columns cropped on the left.
    pub left: u32,
    /// Rows cropped at the top.
    pub top: u32,
    /// The visible width.
    pub width: u32,
    /// The visible height.
    pub height: u32,
}

/// The surfaces of one sequence, which frames of any identity `K` are
/// assigned to, and the means to read them back.
pub struct Target<K> {
    // Declaration order is teardown order; the device goes last.
    image: Image,
    context: Context,
    surfaces: Surfaces,
    _config: Config,
    device: Rc<Device>,
    format: SampleFormat,
    /// The surfaces not holding a frame.
    free: Vec<usize>,
    /// The surface index of each frame.
    assigned: HashMap<K, usize>,
    timestamps: HashMap<K, u64>,
    coded: (u32, u32),
    visible: Visible,
}

impl<K: Hash + Eq + Copy> Target<K> {
    /// Creates `count` surfaces of the coded size and a decode context for
    /// `profile`.
    pub fn new(
        device: &Rc<Device>,
        profile: c_int,
        format: SampleFormat,
        coded: (u32, u32),
        visible: Visible,
        count: usize,
    ) -> Result<Self, VaapiError> {
        let config = create_config(device, profile, format)?;
        let surfaces = Surfaces::create(device, format, coded.0, coded.1, count)?;
        let context = Context::create(device, &config, coded.0, coded.1, &surfaces)?;
        let image = Image::create(device, format, coded.0, coded.1)?;
        Ok(Self {
            image,
            context,
            surfaces,
            _config: config,
            device: Rc::clone(device),
            format,
            free: (0..count).rev().collect(),
            assigned: HashMap::new(),
            timestamps: HashMap::new(),
            coded,
            visible,
        })
    }

    /// The buffers of a picture to render.
    pub fn picture_buffers(&self) -> PictureBuffers {
        PictureBuffers::new(&self.device, self.context.id)
    }

    /// Frees the surface of a frame.
    pub fn release(&mut self, id: K) {
        if let Some(index) = self.assigned.remove(&id) {
            self.free.push(index);
        }
        self.timestamps.remove(&id);
    }

    /// Notes the timestamp of the packet a frame was decoded from.
    pub fn stamp(&mut self, id: K, timestamp_ns: u64) {
        self.timestamps.insert(id, timestamp_ns);
    }

    /// The surface of a frame, taking a free one for a frame that has none.
    pub fn surface_of(&mut self, id: K) -> Result<VASurfaceID, VaapiError> {
        let index = if let Some(&index) = self.assigned.get(&id) {
            index
        } else {
            let index = self.free.pop().ok_or(VaapiError::NoFreeSurface)?;
            self.assigned.insert(id, index);
            index
        };
        Ok(self.surfaces.ids[index])
    }

    /// Reads a decoded frame back, cropped and tightly packed.
    pub fn read(&self, id: K) -> Result<DecodedPicture, VaapiError> {
        let (&index, &timestamp_ns) = self
            .assigned
            .get(&id)
            .zip(self.timestamps.get(&id))
            .ok_or(VaapiError::NeverDecoded)?;
        let device = &*self.device;
        let image = &self.image.image;
        // SAFETY: the surface and the image belong to this display, and the
        // surface was synchronised when its picture was rendered.
        let status = unsafe {
            (device.api.get_image)(
                device.display,
                self.surfaces.ids[index],
                0,
                0,
                self.coded.0,
                self.coded.1,
                image.image_id,
            )
        };
        device.check("vaGetImage", status)?;
        if image.num_planes != 2 {
            return Err(VaapiError::BufferTooSmall("image planes"));
        }
        let planes = Nv12Planes {
            y_offset: to_usize(image.offsets[0])?,
            y_pitch: to_usize(image.pitches[0])?,
            uv_offset: to_usize(image.offsets[1])?,
            uv_pitch: to_usize(image.pitches[1])?,
        };
        let data =
            device.with_mapped_bytes(image.buf, to_usize(image.data_size)?, "image", |bytes| {
                copy_cropped(
                    bytes,
                    planes,
                    self.coded,
                    self.visible,
                    self.format.bytes_per_sample,
                )
            })?;
        Ok(DecodedPicture {
            data,
            width: self.visible.width,
            height: self.visible.height,
            timestamp_ns,
            layout: self.format.layout,
        })
    }
}

fn create_config(
    device: &Rc<Device>,
    profile: c_int,
    format: SampleFormat,
) -> Result<Config, VaapiError> {
    let mut attribute = VAConfigAttrib {
        type_: VAConfigAttribRTFormat,
        value: 0,
    };
    // SAFETY: the pointer refers to the one attribute counted.
    let status = unsafe {
        (device.api.get_config_attributes)(
            device.display,
            profile,
            VAEntrypointVLD,
            &raw mut attribute,
            1,
        )
    };
    device.check("vaGetConfigAttributes", status)?;
    if attribute.value == VA_ATTRIB_NOT_SUPPORTED || attribute.value & format.rt_format == 0 {
        return Err(VaapiError::UnsupportedRtFormat);
    }
    attribute.value = format.rt_format;
    let mut id = VA_INVALID_ID;
    // SAFETY: the pointers refer to live locals; one attribute is counted.
    let status = unsafe {
        (device.api.create_config)(
            device.display,
            profile,
            VAEntrypointVLD,
            &raw mut attribute,
            1,
            &raw mut id,
        )
    };
    device.check("vaCreateConfig", status)?;
    Ok(Config::new(Rc::clone(device), id))
}

/// Whether the device offers the decode entry point for `profile`.
pub fn offers_decoding(device: &Device, profile: c_int) -> Result<bool, VaapiError> {
    Ok(device
        .entrypoints(profile)?
        .is_some_and(|entrypoints| entrypoints.contains(&VAEntrypointVLD)))
}

/// Copies the visible rectangle of a mapped semi-planar image into a tightly
/// packed frame.
pub(super) fn copy_cropped(
    image: &[u8],
    planes: Nv12Planes,
    (coded_width, coded_height): (u32, u32),
    visible: Visible,
    bytes_per_sample: usize,
) -> Result<Vec<u8>, VaapiError> {
    let too_small = || VaapiError::BufferTooSmall("image");
    let (width, height) = (to_usize(visible.width)?, to_usize(visible.height)?);
    let (left, top) = (to_usize(visible.left)?, to_usize(visible.top)?);
    if left + width > to_usize(coded_width)? || top + height > to_usize(coded_height)? {
        return Err(too_small());
    }
    let row_bytes = width * bytes_per_sample;
    let mut frame = Vec::with_capacity(row_bytes * height * 3 / 2);
    for row in 0..height {
        let start = planes.y_offset + (top + row) * planes.y_pitch + left * bytes_per_sample;
        frame.extend_from_slice(image.get(start..start + row_bytes).ok_or_else(too_small)?);
    }
    // A chroma row covers two luma rows, and a chroma sample two luma
    // columns in two samples, so the byte offset of the column is `left`
    // samples.
    for row in 0..height / 2 {
        let start = planes.uv_offset + (top / 2 + row) * planes.uv_pitch + left * bytes_per_sample;
        frame.extend_from_slice(image.get(start..start + row_bytes).ok_or_else(too_small)?);
    }
    Ok(frame)
}
