//! Compatibility layer for different font engines.
//!
//! CoreText is used on Mac OS.
//! FreeType is used on everything that's not Mac OS.
//! Eventually, ClearType support will be available for windows.

#![deny(clippy::all, clippy::if_not_else, clippy::enum_glob_use, clippy::wrong_pub_self_convention)]

use std::fmt::{self, Display, Formatter};
use std::ops::{Add, Mul};
use std::sync::atomic::{AtomicUsize, Ordering};

// If target isn't macos or windows, reexport everything from ft.
#[cfg(not(any(target_os = "macos", windows)))]
pub mod ft;
#[cfg(not(any(target_os = "macos", windows)))]
pub use ft::FreeTypeRasterizer as Rasterizer;

#[cfg(windows)]
pub mod directwrite;
#[cfg(windows)]
pub use directwrite::DirectWriteRasterizer as Rasterizer;

// If target is macos, reexport everything from darwin.
#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "macos")]
pub use darwin::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FontDesc {
    name: String,
    style: Style,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slant {
    Normal,
    Italic,
    Oblique,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Weight {
    Normal,
    Bold,
}

/// Style of font.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Style {
    Specific(String),
    Description { slant: Slant, weight: Weight },
}

impl fmt::Display for Style {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Style::Specific(ref s) => f.write_str(&s),
            Style::Description { slant, weight } => {
                write!(f, "slant={:?}, weight={:?}", slant, weight)
            },
        }
    }
}

impl FontDesc {
    pub fn new<S>(name: S, style: Style) -> FontDesc
    where
        S: Into<String>,
    {
        FontDesc { name: name.into(), style }
    }
}

impl fmt::Display for FontDesc {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} - {}", self.name, self.style)
    }
}

/// Identifier for a Font for use in maps/etc.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub struct FontKey {
    token: u32,
}

impl FontKey {
    /// Get next font key for given size.
    ///
    /// The generated key will be globally unique.
    pub fn next() -> FontKey {
        static TOKEN: AtomicUsize = AtomicUsize::new(0);

        FontKey { token: TOKEN.fetch_add(1, Ordering::SeqCst) as _ }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct GlyphKey {
    pub character: char,
    pub font_key: FontKey,
    pub size: Size,
}

/// Font size stored as integer.
#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct Size(i16);

impl Size {
    /// Create a new `Size` from a f32 size in points.
    pub fn new(size: f32) -> Size {
        Size((size * Size::factor()) as i16)
    }

    /// Scale factor between font "Size" type and point size.
    #[inline]
    pub fn factor() -> f32 {
        2.0
    }

    /// Get the f32 size in points.
    pub fn as_f32_pts(self) -> f32 {
        f32::from(self.0) / Size::factor()
    }
}

impl<T: Into<Size>> Add<T> for Size {
    type Output = Size;

    fn add(self, other: T) -> Size {
        Size(self.0.saturating_add(other.into().0))
    }
}

impl<T: Into<Size>> Mul<T> for Size {
    type Output = Size;

    fn mul(self, other: T) -> Size {
        Size(self.0 * other.into().0)
    }
}

impl From<f32> for Size {
    fn from(float: f32) -> Size {
        Size::new(float)
    }
}

#[derive(Clone)]
pub struct RasterizedGlyph {
    pub character: char,
    pub width: i32,
    pub height: i32,
    pub top: i32,
    pub left: i32,
    pub buffer: BitmapBuffer,
}

#[derive(Clone, Debug)]
pub enum BitmapBuffer {
    /// RGB alphamask.
    RGB(Vec<u8>),

    /// RGBA pixels with premultiplied alpha.
    RGBA(Vec<u8>),
}

impl Default for RasterizedGlyph {
    fn default() -> RasterizedGlyph {
        RasterizedGlyph {
            character: ' ',
            width: 0,
            height: 0,
            top: 0,
            left: 0,
            buffer: BitmapBuffer::RGB(Vec::new()),
        }
    }
}

impl fmt::Debug for RasterizedGlyph {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("RasterizedGlyph")
            .field("character", &self.character)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("top", &self.top)
            .field("left", &self.left)
            .field("buffer", &self.buffer)
            .finish()
    }
}

#[derive(Copy, Clone)]
pub struct Metrics {
    pub average_advance: f64,
    pub line_height: f64,
    pub descent: f32,
    pub underline_position: f32,
    pub underline_thickness: f32,
    pub strikeout_position: f32,
    pub strikeout_thickness: f32,
}

/// Errors occuring when using the rasterizer.
#[derive(Debug)]
pub enum Error {
    /// Unable to find a font matching the description.
    FontNotFound(FontDesc),

    /// Unable to find metrics information on a font face.
    MetricsNotFont,

    /// The glyph could not be found in any font.
    MissingGlyph(RasterizedGlyph),

    /// Requested an operation with a FontKey that isn't known to the rasterizer.
    UnknownFontKey,

    /// Error from platfrom's font system.
    PlatformError(String),
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Error::FontNotFound(font) => write!(f, "font {:?} not found", font),
            Error::MissingGlyph(glyph) => {
                write!(f, "glyph for character {:?} not found", glyph.character)
            },
            Error::UnknownFontKey => f.write_str("invalid font key"),
            Error::MetricsNotFont => {
                f.write_str("unable to find metrics information on a font face")
            },
            Error::PlatformError(err) => write!(f, "{}", err),
        }
    }
}

pub trait Rasterize {
    /// Create a new Rasterizer.
    fn new(device_pixel_ratio: f32, use_thin_strokes: bool) -> Result<Self, Error>
    where
        Self: Sized;

    /// Get `Metrics` for the given `FontKey`.
    fn metrics(&self, _: FontKey, _: Size) -> Result<Metrics, Error>;

    /// Load the font described by `FontDesc` and `Size`.
    fn load_font(&mut self, _: &FontDesc, _: Size) -> Result<FontKey, Error>;

    /// Rasterize the glyph described by `GlyphKey`..
    fn get_glyph(&mut self, _: GlyphKey) -> Result<RasterizedGlyph, Error>;

    /// Update the Rasterizer's DPI factor.
    fn update_dpr(&mut self, device_pixel_ratio: f32);
}
