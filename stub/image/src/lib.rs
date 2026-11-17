//! Minimal stub of the `image` API that eframe (native) and arboard use, so the real crate and its
//! codecs stay out of the exe.
//!
//! eframe uses `image` in two places:
//! 1. Decoding egui's built-in PNG icon when the window has none: never reached, main.rs always
//!    sets a placeholder icon.
//! 2. On Windows, rescaling and re-encoding the icon as PNG for the taskbar: `from_raw` returns None
//!    here, eframe logs it and gives up; `win::setup_window` sets the icons from the exe resource.
//!
//! arboard (eframe's `clipboard` feature) needs it only for image clipboard data (PNG/BMP): every
//! codec here fails, so copying images fails; text is unaffected.
//!
//! If an eframe/arboard upgrade fails to build, add the signatures the errors ask for.

use std::fmt;

#[derive(Debug)]
pub struct ImageError;

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("image support is stubbed out in this build")
    }
}

impl std::error::Error for ImageError {}

pub type ImageResult<T> = Result<T, ImageError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorType {
    Rgba8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtendedColorType {
    Rgba8,
}

pub trait ImageDecoder {
    fn dimensions(&self) -> (u32, u32);
}

pub trait ImageEncoder {
    fn write_image(self, buf: &[u8], width: u32, height: u32, color: ExtendedColorType) -> ImageResult<()>;
}

/// Every decoder fails to construct, so there is never an instance.
pub struct NoDecoder;

impl ImageDecoder for NoDecoder {
    fn dimensions(&self) -> (u32, u32) {
        (0, 0)
    }
}

pub mod codecs {
    pub mod png {
        use crate::*;
        pub type PngDecoder = NoDecoder;
        impl NoDecoder {
            pub fn new<R>(_r: R) -> ImageResult<Self> {
                Err(ImageError)
            }
            pub fn new_without_file_header<R>(_r: R) -> ImageResult<Self> {
                Err(ImageError)
            }
        }
        pub struct PngEncoder<W>(W);
        impl<W> PngEncoder<W> {
            pub fn new(w: W) -> Self {
                Self(w)
            }
        }
        impl<W> ImageEncoder for PngEncoder<W> {
            fn write_image(self, _buf: &[u8], _w: u32, _h: u32, _c: ExtendedColorType) -> ImageResult<()> {
                Err(ImageError)
            }
        }
    }
    pub mod bmp {
        pub type BmpDecoder = crate::NoDecoder;
    }
}

pub struct DynamicImage(RgbaImage);

impl DynamicImage {
    pub fn from_decoder(_decoder: impl ImageDecoder) -> ImageResult<Self> {
        Err(ImageError)
    }
    pub fn into_rgba8(self) -> RgbaImage {
        self.0
    }
}

#[derive(Clone)]
pub struct RgbaImage {
    width: u32,
    height: u32,
    raw: Vec<u8>,
}

impl RgbaImage {
    pub fn from_raw(_width: u32, _height: u32, _raw: Vec<u8>) -> Option<Self> {
        None
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn as_raw(&self) -> &Vec<u8> {
        &self.raw
    }
    pub fn into_raw(self) -> Vec<u8> {
        self.raw
    }
    pub fn write_to<W: std::io::Write + std::io::Seek>(&self, _w: &mut W, _format: ImageFormat) -> ImageResult<()> {
        Err(ImageError)
    }
}

pub fn load_from_memory(_buffer: &[u8]) -> ImageResult<DynamicImage> {
    Err(ImageError)
}

pub fn save_buffer(_path: impl AsRef<std::path::Path>, _buf: &[u8], _w: u32, _h: u32, _color: ColorType) -> ImageResult<()> {
    Err(ImageError)
}

pub mod imageops {
    pub use FilterType::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum FilterType {
        Lanczos3,
    }

    pub fn resize(image: &super::RgbaImage, _w: u32, _h: u32, _filter: FilterType) -> super::RgbaImage {
        image.clone()
    }
}
