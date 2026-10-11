//! Motion JPEG: every packet is a baseline JPEG picture, so every frame is a
//! key frame and playback may skip ahead freely.

use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

use crate::{Decoder, Error, MAX_PIXELS};

#[derive(Default)]
pub struct Mjpeg;

impl Mjpeg {
    pub fn new() -> Mjpeg {
        Mjpeg
    }
}

impl Decoder for Mjpeg {
    fn decode(&mut self, packet: &[u8], rgba: &mut Vec<u8>) -> Result<(u32, u32), Error> {
        let options = DecoderOptions::default()
            .jpeg_set_out_colorspace(ColorSpace::RGBA)
            .set_max_width(8192)
            .set_max_height(8192);
        let mut decoder = JpegDecoder::new_with_options(ZCursor::new(packet), options);
        decoder
            .decode_headers()
            .map_err(|e| Error::Decode(format!("{e:?}")))?;
        let (w, h) = decoder
            .dimensions()
            .ok_or(Error::Corrupt("no picture size"))?;
        if w as u64 * h as u64 > MAX_PIXELS {
            return Err(Error::TooLarge);
        }
        let size = decoder.output_buffer_size().ok_or(Error::TooLarge)?;
        rgba.clear();
        rgba.resize(size, 0);
        decoder
            .decode_into(rgba)
            .map_err(|e| Error::Decode(format!("{e:?}")))?;
        Ok((w as u32, h as u32))
    }
}
