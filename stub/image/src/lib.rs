//! eframe（原生端）用到的 `image` API 的最小存根。
//!
//! eframe 只在两处用 `image`：
//! 1. 没设置窗口图标时解码内置的 egui PNG 图标 —— SRename-rs 总是自己设置图标，走不到；
//! 2. Windows 上把图标缩放并重新编码成 PNG 来设任务栏大图标 —— 这里 `from_raw` 返回 None，
//!    eframe 记一条日志后放弃，大小图标改由 `shell::set_window_icons` 直接用 DIB 创建。
//!
//!
//! 若开了 eframe 的 `clipboard` 功能，arboard 也会依赖 `image`，但只用于**图片**剪贴板
//! （PNG/BMP 编解码）；这里的编解码器全部返回错误，图片复制粘贴会失败，文本剪贴板不受影响。
//!
//! 升级 eframe/arboard 时若编译失败，按报错补齐签名即可。

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

/// 所有解码器都构造失败，所以不会真的产生实例。
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
