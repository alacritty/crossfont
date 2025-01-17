use std::fmt;
use std::ptr;

use foreign_types::{ForeignType, ForeignTypeRef};

use fontconfig_sys as ffi;

use ffi::constants::{FC_SLANT_ITALIC, FC_SLANT_OBLIQUE, FC_SLANT_ROMAN};
use ffi::constants::{FC_WEIGHT_BLACK, FC_WEIGHT_BOLD, FC_WEIGHT_EXTRABLACK, FC_WEIGHT_EXTRABOLD};
use ffi::constants::{FC_WEIGHT_BOOK, FC_WEIGHT_MEDIUM, FC_WEIGHT_REGULAR, FC_WEIGHT_SEMIBOLD};
use ffi::constants::{FC_WEIGHT_EXTRALIGHT, FC_WEIGHT_LIGHT, FC_WEIGHT_THIN};
use ffi::FcInitBringUptoDate;
use ffi::FcResultNoMatch;
use ffi::{FcFontList, FcFontMatch, FcFontSort};
use ffi::{FcMatchFont, FcMatchPattern, FcMatchScan};
use ffi::{FcSetApplication, FcSetSystem};

pub mod config;
pub use config::{Config, ConfigRef};

pub mod font_set;
pub use font_set::{FontSet, FontSetRef};

pub mod object_set;
pub use object_set::{ObjectSet, ObjectSetRef};

pub mod char_set;
pub use char_set::{CharSet, CharSetRef};

pub mod pattern;
pub use pattern::{FtFaceLocation, Pattern, PatternHash, PatternRef};

/// Find the font closest matching the provided pattern.
///
/// The returned pattern is the result of Pattern::render_prepare.
pub fn font_match(config: &ConfigRef, pattern: &PatternRef) -> Option<Pattern> {
    unsafe {
        let mut result = FcResultNoMatch;
        let ptr = FcFontMatch(config.as_ptr(), pattern.as_ptr(), &mut result);

        if ptr.is_null() {
            None
        } else {
            Some(Pattern::from_ptr(ptr))
        }
    }
}

/// Reloads the Fontconfig configuration files.
pub fn update_config() {
    unsafe {
        let _ = FcInitBringUptoDate();
    }
}

/// List fonts by closeness to the pattern.
pub fn font_sort(config: &ConfigRef, pattern: &PatternRef) -> Option<FontSet> {
    let ptr = unsafe {
        let mut result = FcResultNoMatch;
        FcFontSort(
            config.as_ptr(),
            pattern.as_ptr(),
            1, // Trim font list.
            ptr::null_mut(),
            &mut result,
        )
    };

    if ptr.is_null() {
        None
    } else {
        Some(unsafe { FontSet::from_ptr(ptr) })
    }
}

/// List fonts matching pattern.
pub fn font_list(
    config: &ConfigRef,
    pattern: &PatternRef,
    objects: &ObjectSetRef,
) -> Option<FontSet> {
    unsafe {
        let ptr = FcFontList(config.as_ptr(), pattern.as_ptr(), objects.as_ptr());

        if ptr.is_null() {
            None
        } else {
            Some(FontSet::from_ptr(ptr))
        }
    }
}

/// Available font sets.
#[derive(Debug, Copy, Clone)]
pub enum SetName {
    System = FcSetSystem as isize,
    Application = FcSetApplication as isize,
}

/// When matching, how to match.
#[derive(Debug, Copy, Clone)]
pub enum MatchKind {
    Font = FcMatchFont as isize,
    Pattern = FcMatchPattern as isize,
    Scan = FcMatchScan as isize,
}

#[derive(Debug, Copy, Clone)]
pub enum Slant {
    Italic = FC_SLANT_ITALIC as isize,
    Oblique = FC_SLANT_OBLIQUE as isize,
    Roman = FC_SLANT_ROMAN as isize,
}

#[derive(Debug, Copy, Clone)]
pub enum Weight {
    Thin = FC_WEIGHT_THIN as isize,
    Extralight = FC_WEIGHT_EXTRALIGHT as isize,
    Light = FC_WEIGHT_LIGHT as isize,
    Book = FC_WEIGHT_BOOK as isize,
    Regular = FC_WEIGHT_REGULAR as isize,
    Medium = FC_WEIGHT_MEDIUM as isize,
    Semibold = FC_WEIGHT_SEMIBOLD as isize,
    Bold = FC_WEIGHT_BOLD as isize,
    Extrabold = FC_WEIGHT_EXTRABOLD as isize,
    Black = FC_WEIGHT_BLACK as isize,
    Extrablack = FC_WEIGHT_EXTRABLACK as isize,
}

#[derive(Debug, Copy, Clone)]
pub enum Width {
    Ultracondensed,
    Extracondensed,
    Condensed,
    Semicondensed,
    Normal,
    Semiexpanded,
    Expanded,
    Extraexpanded,
    Ultraexpanded,
    Other(i32),
}

impl Width {
    fn to_isize(self) -> isize {
        match self {
            Width::Ultracondensed => 50,
            Width::Extracondensed => 63,
            Width::Condensed => 75,
            Width::Semicondensed => 87,
            Width::Normal => 100,
            Width::Semiexpanded => 113,
            Width::Expanded => 125,
            Width::Extraexpanded => 150,
            Width::Ultraexpanded => 200,
            Width::Other(value) => value as isize,
        }
    }
}

impl From<isize> for Width {
    fn from(value: isize) -> Self {
        match value {
            50 => Width::Ultracondensed,
            63 => Width::Extracondensed,
            75 => Width::Condensed,
            87 => Width::Semicondensed,
            100 => Width::Normal,
            113 => Width::Semiexpanded,
            125 => Width::Expanded,
            150 => Width::Extraexpanded,
            200 => Width::Ultraexpanded,
            _ => Width::Other(value as _),
        }
    }
}

/// Subpixel geometry.
#[derive(Debug)]
pub enum Rgba {
    Unknown,
    Rgb,
    Bgr,
    Vrgb,
    Vbgr,
    None,
}

impl Rgba {
    fn to_isize(&self) -> isize {
        match *self {
            Rgba::Unknown => 0,
            Rgba::Rgb => 1,
            Rgba::Bgr => 2,
            Rgba::Vrgb => 3,
            Rgba::Vbgr => 4,
            Rgba::None => 5,
        }
    }
}

impl fmt::Display for Rgba {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match *self {
            Rgba::Unknown => "unknown",
            Rgba::Rgb => "rgb",
            Rgba::Bgr => "bgr",
            Rgba::Vrgb => "vrgb",
            Rgba::Vbgr => "vbgr",
            Rgba::None => "none",
        })
    }
}

impl From<isize> for Rgba {
    fn from(val: isize) -> Rgba {
        match val {
            1 => Rgba::Rgb,
            2 => Rgba::Bgr,
            3 => Rgba::Vrgb,
            4 => Rgba::Vbgr,
            5 => Rgba::None,
            _ => Rgba::Unknown,
        }
    }
}

/// Hinting Style.
#[derive(Debug, Copy, Clone)]
pub enum HintStyle {
    None,
    Slight,
    Medium,
    Full,
}

impl fmt::Display for HintStyle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match *self {
            HintStyle::None => "none",
            HintStyle::Slight => "slight",
            HintStyle::Medium => "medium",
            HintStyle::Full => "full",
        })
    }
}

/// Lcd filter, used to reduce color fringing with subpixel rendering.
pub enum LcdFilter {
    None,
    Default,
    Light,
    Legacy,
}

impl fmt::Display for LcdFilter {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match *self {
            LcdFilter::None => "none",
            LcdFilter::Default => "default",
            LcdFilter::Light => "light",
            LcdFilter::Legacy => "legacy",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_match() {
        let mut pattern = Pattern::new();
        pattern.add_family("monospace");
        pattern.add_style("regular");

        let config = Config::get_current();
        pattern.config_substitute(config, MatchKind::Pattern);
        pattern.default_substitute();
        let font = super::font_match(config, &pattern).expect("match font monospace");

        print!("index={:?}; ", font.index());
        print!("family={:?}; ", font.family());
        print!("style={:?}; ", font.style());
        print!("antialias={:?}; ", font.antialias());
        print!("autohint={:?}; ", font.autohint());
        print!("hinting={:?}; ", font.hinting());
        print!("rgba={:?}; ", font.rgba());
        print!("embeddedbitmap={:?}; ", font.embeddedbitmap());
        print!("lcdfilter={:?}; ", font.lcdfilter());
        print!("hintstyle={:?}", font.hintstyle());
        println!();
    }

    #[test]
    fn font_sort() {
        let mut pattern = Pattern::new();
        pattern.add_family("monospace");
        pattern.set_slant(Slant::Italic);

        let config = Config::get_current();
        pattern.config_substitute(config, MatchKind::Pattern);
        pattern.default_substitute();
        let fonts = super::font_sort(config, &pattern).expect("sort font monospace");

        for font in fonts.into_iter().take(10) {
            let font = pattern.render_prepare(config, font);
            print!("index={:?}; ", font.index());
            print!("family={:?}; ", font.family());
            print!("style={:?}; ", font.style());
            print!("rgba={:?}", font.rgba());
            print!("rgba={:?}", font.rgba());
            println!();
        }
    }

    #[test]
    fn font_sort_with_glyph() {
        let mut charset = CharSet::new();
        charset.add('💖');
        let mut pattern = Pattern::new();
        pattern.add_charset(&charset);
        drop(charset);

        let config = Config::get_current();
        pattern.config_substitute(config, MatchKind::Pattern);
        pattern.default_substitute();
        let fonts = super::font_sort(config, &pattern).expect("font_sort");

        for font in fonts.into_iter().take(10) {
            let font = pattern.render_prepare(config, font);
            print!("index={:?}; ", font.index());
            print!("family={:?}; ", font.family());
            print!("style={:?}; ", font.style());
            print!("rgba={:?}", font.rgba());
            println!();
        }
    }
}
