//! Font rendering based on CoreText.

use std::collections::HashMap;
use std::ffi::{c_void, CStr};
use std::iter;
use std::path::PathBuf;
use std::ptr::{self, NonNull};
use std::slice;

use objc2::rc::autoreleasepool;
use objc2_core_foundation::{
    kCFTypeSetCallBacks, CFArray, CFDictionary, CFIndex, CFNumber, CFRetained, CFSet, CFString,
    CGFloat, CGPoint, CGRect, CGSize, Type, CFURL,
};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextGetBytesPerRow, CGBitmapContextGetData,
    CGBitmapContextGetHeight, CGBitmapInfo, CGColorSpace, CGContext, CGGlyph, CGImageAlphaInfo,
};
use objc2_core_text::{
    kCTFontCollectionRemoveDuplicatesOption, kCTFontEnabledAttribute, kCTFontFamilyNameAttribute,
    kCTFontStyleNameAttribute, kCTFontURLAttribute, CTFont, CTFontCollection, CTFontDescriptor,
    CTFontOrientation, CTFontSymbolicTraits,
};
use objc2_foundation::{ns_string, NSNumber, NSUserDefaults};

use log::{trace, warn};
use once_cell::sync::Lazy;

pub mod byte_order;

use super::{
    BitmapBuffer, Error, FontDesc, FontKey, GlyphKey, Metrics, RasterizedGlyph, Size, Slant, Style,
    Weight,
};

/// According to the documentation, the index of 0 must be a missing glyph character:
/// https://developer.apple.com/fonts/TrueType-Reference-Manual/RM07/appendixB.html
const MISSING_GLYPH_INDEX: u32 = 0;

/// Font descriptor.
///
/// The descriptor provides data about a font and supports creating a font.
#[derive(Debug)]
struct Descriptor {
    style_name: String,
    font_path: PathBuf,

    ct_descriptor: CFRetained<CTFontDescriptor>,
}

impl Descriptor {
    fn new(desc: &CTFontDescriptor) -> Descriptor {
        let style_name = unsafe { desc.attribute(kCTFontStyleNameAttribute) }
            .expect("a font must have a non-null style name")
            .downcast::<CFString>()
            .unwrap();
        let font_path = unsafe { desc.attribute(kCTFontURLAttribute) }
            .and_then(|attr| attr.downcast::<CFURL>().ok())
            .map(|url| url.to_file_path().unwrap())
            .unwrap_or_default();
        Descriptor { style_name: style_name.to_string(), font_path, ct_descriptor: desc.retain() }
    }

    /// Create a Font from this descriptor.
    fn to_font(&self, size: f64, load_fallbacks: bool) -> Font {
        let ct_font = unsafe {
            CTFont::with_font_descriptor(&self.ct_descriptor, size as CGFloat, ptr::null())
        };

        let fallbacks = if load_fallbacks {
            // TODO fixme, hardcoded en for english.
            let mut fallbacks = cascade_list_for_languages(&ct_font, &["en".to_owned()])
                .into_iter()
                .filter(|desc| !desc.font_path.as_os_str().is_empty())
                .map(|desc| desc.to_font(size, false))
                .collect::<Vec<_>>();

            // TODO, we can't use apple's proposed
            // .Apple Symbol Fallback (filtered out below),
            // but not having these makes us not able to render
            // many chars. We add the symbols back in.
            // Investigate if we can actually use the .-prefixed
            // fallbacks somehow.
            let name = CFString::from_static_str("Apple Symbols");
            let apple_symbols = unsafe { CTFont::with_name(&name, size, ptr::null_mut()) };
            fallbacks.push(Font { ct_font: apple_symbols, fallbacks: Vec::new() });

            fallbacks
        } else {
            Vec::new()
        };

        Font { ct_font, fallbacks }
    }
}

/// CoreTextRasterizer, the main type exported by this package.
///
/// Given a fontdesc, can rasterize fonts.
pub struct CoreTextRasterizer {
    fonts: HashMap<FontKey, Font>,
    keys: HashMap<(FontDesc, Size), FontKey>,
}

impl crate::Rasterize for CoreTextRasterizer {
    fn new() -> Result<CoreTextRasterizer, Error> {
        Ok(CoreTextRasterizer { fonts: HashMap::new(), keys: HashMap::new() })
    }

    /// Get metrics for font specified by FontKey.
    fn metrics(&self, key: FontKey, _size: Size) -> Result<Metrics, Error> {
        let font = self.fonts.get(&key).ok_or(Error::UnknownFontKey)?;

        Ok(font.metrics())
    }

    fn load_font(&mut self, desc: &FontDesc, size: Size) -> Result<FontKey, Error> {
        let size = Size::new(size.as_pt());
        self.keys.get(&(desc.to_owned(), size)).map(|k| Ok(*k)).unwrap_or_else(|| {
            let font = self.get_font(desc, size)?;
            let key = FontKey::next();

            self.fonts.insert(key, font);
            self.keys.insert((desc.clone(), size), key);

            Ok(key)
        })
    }

    /// Get rasterized glyph for given glyph key.
    fn get_glyph(&mut self, glyph: GlyphKey) -> Result<RasterizedGlyph, Error> {
        // Get loaded font.
        let font = self.fonts.get(&glyph.font_key).ok_or(Error::UnknownFontKey)?;

        // Find a font where the given character is present.
        let (font, glyph_index) = iter::once(font)
            .chain(font.fallbacks.iter())
            .find_map(|font| match font.glyph_index(glyph.character) {
                MISSING_GLYPH_INDEX => None,
                glyph_index => Some((font, glyph_index)),
            })
            .unwrap_or((font, MISSING_GLYPH_INDEX));

        let glyph = font.get_glyph(glyph.character, glyph_index);

        if glyph_index == MISSING_GLYPH_INDEX {
            Err(Error::MissingGlyph(glyph))
        } else {
            Ok(glyph)
        }
    }

    fn kerning(&mut self, _left: GlyphKey, _right: GlyphKey) -> (f32, f32) {
        (0., 0.)
    }
}

impl CoreTextRasterizer {
    fn get_specific_face(
        &mut self,
        desc: &FontDesc,
        style: &str,
        size: Size,
    ) -> Result<Font, Error> {
        let descriptors = descriptors_for_family(&desc.name[..]);
        for descriptor in descriptors {
            if descriptor.style_name == style {
                // Found the font we want.
                let size = f64::from(size.as_pt());
                let font = descriptor.to_font(size, true);
                return Ok(font);
            }
        }

        Err(Error::FontNotFound(desc.to_owned()))
    }

    fn get_matching_face(
        &mut self,
        desc: &FontDesc,
        slant: Slant,
        weight: Weight,
        size: Size,
    ) -> Result<Font, Error> {
        let bold = weight == Weight::Bold;
        let italic = slant != Slant::Normal;
        let size = f64::from(size.as_pt());

        let descriptors = descriptors_for_family(&desc.name[..]);
        for descriptor in descriptors {
            let font = descriptor.to_font(size, true);
            if font.is_bold() == bold && font.is_italic() == italic {
                // Found the font we want.
                return Ok(font);
            }
        }

        Err(Error::FontNotFound(desc.to_owned()))
    }

    fn get_font(&mut self, desc: &FontDesc, size: Size) -> Result<Font, Error> {
        match desc.style {
            Style::Specific(ref style) => self.get_specific_face(desc, style, size),
            Style::Description { slant, weight } => {
                self.get_matching_face(desc, slant, weight, size)
            },
        }
    }
}

/// Return fallback descriptors for font/language list.
fn cascade_list_for_languages(ct_font: &CTFont, languages: &[String]) -> Vec<Descriptor> {
    // Convert language type &Vec<String> -> CFArray.
    let langarr = {
        let tmp: Vec<_> = languages.iter().map(|language| CFString::from_str(language)).collect();
        CFArray::from_retained_objects(&tmp)
    };

    let list =
        unsafe { ct_font.default_cascade_list_for_languages(Some(langarr.as_opaque())) }.unwrap();
    // CFArray of CTFontDescriptorRef.
    let list = unsafe { CFRetained::cast_unchecked::<CFArray<CTFontDescriptor>>(list) };

    // Convert CFArray to Vec<Descriptor>.
    list.iter().filter(|desc| is_enabled(desc)).map(|desc| Descriptor::new(&desc)).collect()
}

/// Check if a font is enabled.
fn is_enabled(fontdesc: &CTFontDescriptor) -> bool {
    let Some(attr_val) = (unsafe { fontdesc.attribute(kCTFontEnabledAttribute) }) else {
        return false;
    };

    let Ok(attr_val) = attr_val.downcast::<CFNumber>() else {
        return false;
    };

    attr_val.as_i32().unwrap_or(0) != 0
}

/// Get descriptors for family name.
fn descriptors_for_family(family: &str) -> Vec<Descriptor> {
    let mut out = Vec::new();

    trace!("Family: {}", family);
    let ct_collection = create_for_family(family).unwrap_or_else(|| {
        // Fallback to Menlo if we can't find the config specified font family.
        warn!("Unable to load specified font {}, falling back to Menlo", &family);
        create_for_family("Menlo").expect("Menlo exists")
    });

    let descriptors = unsafe { ct_collection.matching_font_descriptors() };
    if let Some(descriptors) = descriptors {
        // CFArray of CTFontDescriptorRef (i think).
        let descriptors =
            unsafe { CFRetained::cast_unchecked::<CFArray<CTFontDescriptor>>(descriptors) };

        for descriptor in descriptors {
            out.push(Descriptor::new(&descriptor));
        }
    }

    out
}

pub fn create_for_family(family: &str) -> Option<CFRetained<CTFontCollection>> {
    let family_attr = unsafe { kCTFontFamilyNameAttribute };
    let family_name = CFString::from_str(family);
    let specified_attrs = CFDictionary::from_slices(&[family_attr], &[&*family_name]);

    let wildcard_desc = unsafe { CTFontDescriptor::with_attributes(specified_attrs.as_opaque()) };
    let ptr = [family_attr].as_ptr().cast_mut().cast::<*const c_void>();
    let mandatory_attrs = unsafe { CFSet::new(None, ptr, 1, &kCFTypeSetCallBacks).unwrap() };
    let matched_descs = unsafe {
        CTFontDescriptor::matching_font_descriptors(&wildcard_desc, Some(&mandatory_attrs))
    }?;
    let key = unsafe { kCTFontCollectionRemoveDuplicatesOption };
    let value = CFNumber::new_i64(1);
    let options = CFDictionary::from_slices(&[key], &[&*value]);
    Some(unsafe {
        CTFontCollection::with_font_descriptors(Some(&matched_descs), Some(&options.as_opaque()))
    })
}

// The AppleFontSmoothing user default controls font smoothing on macOS, which increases the stroke
// width. By default it is unset, and the system behaves as though it is set to 2, which means a
// medium level of font smoothing. The valid values are integers from 0 to 3. Any other type,
// including a boolean, does not change the behavior. The Core Graphics call we use only supports
// enabling or disabling font smoothing, so we will treat an integer 0 as disabling it, and any
// other integer, or a missing value (the default), or a value of any other type, as leaving it
// enabled.
static FONT_SMOOTHING_ENABLED: Lazy<bool> = Lazy::new(|| {
    autoreleasepool(|_| {
        let value = unsafe {
            NSUserDefaults::standardUserDefaults().objectForKey(ns_string!("AppleFontSmoothing"))
        };

        let value = match value {
            Some(value) => value,
            None => return true,
        };

        if let Some(value) = value.downcast_ref::<NSNumber>() {
            // NSNumber's objCType method returns one of these strings depending on the size:
            // q = quad (long long), l = long, i = int, s = short.
            // This is done to reject booleans, which are NSNumbers with an objCType of "c", but
            // macOS does not treat them the same as an integer 0 or 1 for this setting,
            // it just ignores it.
            let int_specifiers: [&[u8]; 4] = [b"q", b"l", b"i", b"s"];

            let encoding = unsafe { CStr::from_ptr(value.objCType().as_ptr()).to_bytes() };
            if !int_specifiers.contains(&encoding) {
                return true;
            }

            let smoothing = value.integerValue();
            smoothing != 0
        } else if let Ok(value) = value.downcast::<NSNumber>() {
            let smoothing = value.integerValue();
            smoothing != 0
        } else {
            true
        }
    })
});

/// A font.
#[derive(Clone)]
struct Font {
    ct_font: CFRetained<CTFont>,
    fallbacks: Vec<Font>,
}

unsafe impl Send for Font {}

impl Font {
    fn metrics(&self) -> Metrics {
        let average_advance = self.glyph_advance('0');

        let ascent = unsafe { self.ct_font.ascent() }.round();
        let descent = unsafe { self.ct_font.descent() }.round();
        let leading = unsafe { self.ct_font.leading() }.round();
        let line_height = ascent + descent + leading;

        // Strikeout and underline metrics.
        // CoreText doesn't provide strikeout so we provide our own.
        let underline_position = unsafe { self.ct_font.underline_position() } as f32;
        let underline_thickness = unsafe { self.ct_font.underline_thickness() } as f32;
        let strikeout_position = (line_height / 2. - descent) as f32;
        let strikeout_thickness = underline_thickness;

        Metrics {
            average_advance,
            line_height,
            descent: -(descent as f32),
            underline_position,
            underline_thickness,
            strikeout_position,
            strikeout_thickness,
        }
    }

    fn is_bold(&self) -> bool {
        unsafe { self.ct_font.symbolic_traits() }.contains(CTFontSymbolicTraits::BoldTrait)
    }

    fn is_italic(&self) -> bool {
        unsafe { self.ct_font.symbolic_traits() }.contains(CTFontSymbolicTraits::ItalicTrait)
    }

    fn is_colored(&self) -> bool {
        unsafe { self.ct_font.symbolic_traits() }.contains(CTFontSymbolicTraits::ColorGlyphsTrait)
    }

    fn glyph_advance(&self, character: char) -> f64 {
        let index = self.glyph_index(character);

        let indices = [index as CGGlyph];

        unsafe {
            self.ct_font.advances_for_glyphs(
                CTFontOrientation::Default,
                NonNull::from(&indices[0]),
                ptr::null_mut(),
                1,
            )
        }
    }

    fn get_glyph(&self, character: char, glyph_index: u32) -> RasterizedGlyph {
        let glyphs = [glyph_index as CGGlyph];
        let bounds = unsafe {
            self.ct_font.bounding_rects_for_glyphs(
                CTFontOrientation::Default,
                NonNull::from(&glyphs[0]),
                ptr::null_mut(),
                1,
            )
        };

        let rasterized_left = bounds.origin.x.floor() as i32;
        let rasterized_width =
            (bounds.origin.x - f64::from(rasterized_left) + bounds.size.width).ceil() as u32;
        let rasterized_descent = (-bounds.origin.y).ceil() as i32;
        let rasterized_ascent = (bounds.size.height + bounds.origin.y).ceil() as i32;
        let rasterized_height = (rasterized_descent + rasterized_ascent) as u32;

        if rasterized_width == 0 || rasterized_height == 0 {
            return RasterizedGlyph {
                character: ' ',
                width: 0,
                height: 0,
                top: 0,
                left: 0,
                advance: (0, 0),
                buffer: BitmapBuffer::Rgb(Vec::new()),
            };
        }

        let cg_context = unsafe {
            CGBitmapContextCreate(
                ptr::null_mut(),
                rasterized_width as usize,
                rasterized_height as usize,
                8, // bits per component
                rasterized_width as usize * 4,
                CGColorSpace::new_device_rgb().as_deref(),
                CGImageAlphaInfo::PremultipliedFirst.0 | CGBitmapInfo::ByteOrder32Host.0,
            )
        }
        .unwrap();

        let is_colored = self.is_colored();

        // Set background color for graphics context.
        let bg_a = if is_colored { 0.0 } else { 1.0 };
        unsafe { CGContext::set_rgb_fill_color(Some(&cg_context), 0.0, 0.0, 0.0, bg_a) };

        let context_rect = CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(f64::from(rasterized_width), f64::from(rasterized_height)),
        );

        unsafe { CGContext::fill_rect(Some(&cg_context), context_rect) };

        unsafe {
            CGContext::set_allows_font_smoothing(Some(&cg_context), true);
            CGContext::set_should_smooth_fonts(Some(&cg_context), *FONT_SMOOTHING_ENABLED);
            CGContext::set_allows_font_subpixel_quantization(Some(&cg_context), true);
            CGContext::set_should_subpixel_quantize_fonts(Some(&cg_context), true);
            CGContext::set_allows_font_subpixel_positioning(Some(&cg_context), true);
            CGContext::set_should_subpixel_position_fonts(Some(&cg_context), true);
            CGContext::set_allows_antialiasing(Some(&cg_context), true);
            CGContext::set_should_antialias(Some(&cg_context), true);
        }

        // Set fill color to white for drawing the glyph.
        unsafe { CGContext::set_rgb_fill_color(Some(&cg_context), 1.0, 1.0, 1.0, 1.0) };
        let rasterization_origin =
            CGPoint { x: f64::from(-rasterized_left), y: f64::from(rasterized_descent) };

        unsafe {
            self.ct_font.draw_glyphs(
                NonNull::from(&[glyph_index as CGGlyph][0]),
                NonNull::from(&[rasterization_origin][0]),
                1,
                &cg_context,
            )
        };

        let rasterized_pixels = unsafe {
            slice::from_raw_parts_mut(
                CGBitmapContextGetData(Some(&cg_context)) as *mut u8,
                CGBitmapContextGetHeight(Some(&cg_context))
                    * CGBitmapContextGetBytesPerRow(Some(&cg_context)),
            )
        };
        let rasterized_pixels = rasterized_pixels.to_vec();

        let buffer = if is_colored {
            BitmapBuffer::Rgba(byte_order::extract_rgba(&rasterized_pixels))
        } else {
            BitmapBuffer::Rgb(byte_order::extract_rgb(&rasterized_pixels))
        };

        RasterizedGlyph {
            character,
            left: rasterized_left,
            top: (bounds.size.height + bounds.origin.y).ceil() as i32,
            width: rasterized_width as i32,
            height: rasterized_height as i32,
            advance: (0, 0),
            buffer,
        }
    }

    fn glyph_index(&self, character: char) -> u32 {
        // Encode this char as utf-16.
        let mut buffer = [0; 2];
        let encoded: &[u16] = character.encode_utf16(&mut buffer);
        // And use the utf-16 buffer to get the index.
        self.glyph_index_utf16(encoded)
    }

    fn glyph_index_utf16(&self, encoded: &[u16]) -> u32 {
        // Output buffer for the glyph. for non-BMP glyphs, like
        // emojis, this will be filled with two chars the second
        // always being a 0.
        let mut glyphs: [CGGlyph; 2] = [0; 2];

        let res = unsafe {
            self.ct_font.glyphs_for_characters(
                NonNull::new(encoded.as_ptr().cast_mut()).unwrap(),
                NonNull::new(glyphs.as_mut_ptr()).unwrap(),
                encoded.len() as CFIndex,
            )
        };

        if res {
            u32::from(glyphs[0])
        } else {
            MISSING_GLYPH_INDEX
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BitmapBuffer;

    #[test]
    fn get_descriptors_and_build_font() {
        let list = super::descriptors_for_family("Menlo");
        assert!(!list.is_empty());
        println!("{:?}", list);

        // Check to_font.
        let fonts = list.iter().map(|desc| desc.to_font(72., false)).collect::<Vec<_>>();

        for font in fonts {
            // Get a glyph.
            for character in &['a', 'b', 'c', 'd'] {
                let glyph_index = font.glyph_index(*character);
                let glyph = font.get_glyph(*character, glyph_index);

                let buffer = match &glyph.buffer {
                    BitmapBuffer::Rgb(buffer) | BitmapBuffer::Rgba(buffer) => buffer,
                };

                // Debug the glyph.. sigh.
                for row in 0..glyph.height {
                    for col in 0..glyph.width {
                        let index = ((glyph.width * 3 * row) + (col * 3)) as usize;
                        let value = buffer[index];
                        let c = match value {
                            0..=50 => ' ',
                            51..=100 => '.',
                            101..=150 => '~',
                            151..=200 => '*',
                            201..=255 => '#',
                        };
                        print!("{}", c);
                    }
                    println!();
                }
            }
        }
    }
}
