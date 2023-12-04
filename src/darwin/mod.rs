//! Font rendering based on CoreText.

use std::collections::HashMap;
use std::ffi::CStr;
use std::iter;
use std::path::PathBuf;
use std::ptr;

use cocoa::base::{id, nil};
use cocoa::foundation::{NSString, NSUserDefaults};

use core_foundation::array::{CFArray, CFIndex};
use core_foundation::base::{CFType, ItemRef, TCFType};
use core_foundation::number::{CFNumber, CFNumberRef};
use core_foundation::string::CFString;
use core_graphics::base::kCGImageAlphaPremultipliedFirst;
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use core_graphics::font::CGGlyph;
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_text::font::{
    cascade_list_for_languages as ct_cascade_list_for_languages,
    new_from_descriptor as ct_new_from_descriptor, new_from_name, CTFont,
};
use core_text::font_collection::create_for_family;
use core_text::font_descriptor::{
    self, kCTFontColorGlyphsTrait, kCTFontDefaultOrientation, kCTFontEnabledAttribute,
    CTFontDescriptor, SymbolicTraitAccessors,
};

use log::{trace, warn};
use objc::rc::autoreleasepool;
use objc::{class, msg_send, sel, sel_impl};
use once_cell::sync::Lazy;

pub mod byte_order;
use byte_order::kCGBitmapByteOrder32Host;

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

    ct_descriptor: CTFontDescriptor,
}

impl Descriptor {
    fn new(desc: CTFontDescriptor) -> Descriptor {
        Descriptor {
            style_name: desc.style_name(),
            font_path: desc.font_path().unwrap_or_else(PathBuf::new),
            ct_descriptor: desc,
        }
    }

    /// Create a Font from this descriptor.
    fn to_font(&self, size: f64, load_fallbacks: bool) -> Font {
        let ct_font = ct_new_from_descriptor(&self.ct_descriptor, size);

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
            if let Ok(apple_symbols) = new_from_name("Apple Symbols", size) {
                fallbacks.push(Font { ct_font: apple_symbols, fallbacks: Vec::new() })
            };

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
    let langarr: CFArray<CFString> = {
        let tmp: Vec<CFString> = languages.iter().map(|language| CFString::new(language)).collect();
        CFArray::from_CFTypes(&tmp)
    };

    // CFArray of CTFontDescriptorRef (again).
    let list = ct_cascade_list_for_languages(ct_font, &langarr);

    // Convert CFArray to Vec<Descriptor>.
    list.into_iter().filter(is_enabled).map(|fontdesc| Descriptor::new(fontdesc.clone())).collect()
}

/// Check if a font is enabled.
fn is_enabled(fontdesc: &ItemRef<'_, CTFontDescriptor>) -> bool {
    unsafe {
        let descriptor = fontdesc.as_concrete_TypeRef();
        let attr_val =
            font_descriptor::CTFontDescriptorCopyAttribute(descriptor, kCTFontEnabledAttribute);

        if attr_val.is_null() {
            return false;
        }

        let attr_val = CFType::wrap_under_create_rule(attr_val);
        let attr_val = CFNumber::wrap_under_get_rule(attr_val.as_CFTypeRef() as CFNumberRef);

        attr_val.to_i32().unwrap_or(0) != 0
    }
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

    // CFArray of CTFontDescriptorRef (i think).
    let descriptors = ct_collection.get_descriptors();
    if let Some(descriptors) = descriptors {
        for descriptor in descriptors.iter() {
            out.push(Descriptor::new(descriptor.clone()));
        }
    }

    out
}

// The AppleFontSmoothing user default controls font smoothing on macOS, which increases the stroke
// width. By default it is unset, and the system behaves as though it is set to 2, which means a
// medium level of font smoothing. The valid values are integers from 0 to 3. Any other type,
// including a boolean, does not change the behavior. The Core Graphics call we use only supports
// enabling or disabling font smoothing, so we will treat an integer 0 as disabling it, and any
// other integer, or a missing value (the default), or a value of any other type, as leaving it
// enabled.
static FONT_SMOOTHING_ENABLED: Lazy<bool> = Lazy::new(|| {
    autoreleasepool(|| unsafe {
        let key = NSString::alloc(nil).init_str("AppleFontSmoothing");
        let value: id = msg_send![id::standardUserDefaults(), objectForKey: key];

        if !msg_send![value, isKindOfClass: class!(NSNumber)] {
            return true;
        }

        let num_type: id = msg_send![value, objCType];
        if num_type == nil {
            return true;
        }

        // NSNumber's objCType method returns one of these strings depending on the size:
        // q = quad (long long), l = long, i = int, s = short.
        // This is done to reject booleans, which are NSNumbers with an objCType of "c", but macOS
        // does not treat them the same as an integer 0 or 1 for this setting, it just ignores it.
        let int_specifiers: [&[u8]; 4] = [b"q", b"l", b"i", b"s"];
        if !int_specifiers.contains(&CStr::from_ptr(num_type as *const i8).to_bytes()) {
            return true;
        }

        let smoothing: id = msg_send![value, integerValue];
        smoothing as i64 != 0
    })
});

/// A font.
#[derive(Clone)]
struct Font {
    ct_font: CTFont,
    fallbacks: Vec<Font>,
}

unsafe impl Send for Font {}

impl Font {
    fn metrics(&self) -> Metrics {
        let average_advance = self.glyph_advance('0');

        let ascent = self.ct_font.ascent().round() as f64;
        let descent = self.ct_font.descent().round() as f64;
        let leading = self.ct_font.leading().round() as f64;
        let line_height = ascent + descent + leading;

        // Strikeout and underline metrics.
        // CoreText doesn't provide strikeout so we provide our own.
        let underline_position = self.ct_font.underline_position() as f32;
        let underline_thickness = self.ct_font.underline_thickness() as f32;
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
        self.ct_font.symbolic_traits().is_bold()
    }

    fn is_italic(&self) -> bool {
        self.ct_font.symbolic_traits().is_italic()
    }

    fn is_colored(&self) -> bool {
        (self.ct_font.symbolic_traits() & kCTFontColorGlyphsTrait) != 0
    }

    fn glyph_advance(&self, character: char) -> f64 {
        let index = self.glyph_index(character);

        let indices = [index as CGGlyph];

        unsafe {
            self.ct_font.get_advances_for_glyphs(
                kCTFontDefaultOrientation,
                &indices[0],
                ptr::null_mut(),
                1,
            )
        }
    }

    fn get_glyph(&self, character: char, glyph_index: u32) -> RasterizedGlyph {
        let bounds = self
            .ct_font
            .get_bounding_rects_for_glyphs(kCTFontDefaultOrientation, &[glyph_index as CGGlyph]);

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

        let mut cg_context = CGContext::create_bitmap_context(
            None,
            rasterized_width as usize,
            rasterized_height as usize,
            8, // bits per component
            rasterized_width as usize * 4,
            &CGColorSpace::create_device_rgb(),
            kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Host,
        );

        let is_colored = self.is_colored();

        // Set background color for graphics context.
        let bg_a = if is_colored { 0.0 } else { 1.0 };
        cg_context.set_rgb_fill_color(0.0, 0.0, 0.0, bg_a);

        let context_rect = CGRect::new(
            &CGPoint::new(0.0, 0.0),
            &CGSize::new(f64::from(rasterized_width), f64::from(rasterized_height)),
        );

        cg_context.fill_rect(context_rect);

        cg_context.set_allows_font_smoothing(true);
        cg_context.set_should_smooth_fonts(*FONT_SMOOTHING_ENABLED);
        cg_context.set_allows_font_subpixel_quantization(true);
        cg_context.set_should_subpixel_quantize_fonts(true);
        cg_context.set_allows_font_subpixel_positioning(true);
        cg_context.set_should_subpixel_position_fonts(true);
        cg_context.set_allows_antialiasing(true);
        cg_context.set_should_antialias(true);

        // Set fill color to white for drawing the glyph.
        cg_context.set_rgb_fill_color(1.0, 1.0, 1.0, 1.0);
        let rasterization_origin =
            CGPoint { x: f64::from(-rasterized_left), y: f64::from(rasterized_descent) };

        self.ct_font.draw_glyphs(
            &[glyph_index as CGGlyph],
            &[rasterization_origin],
            cg_context.clone(),
        );

        let rasterized_pixels = cg_context.data().to_vec();

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
            self.ct_font.get_glyphs_for_characters(
                encoded.as_ptr(),
                glyphs.as_mut_ptr(),
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
