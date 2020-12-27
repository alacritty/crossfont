//! Rasterization powered by DirectWrite.

use libc::c_void;
use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;

use dwrote::{
    FontCollection, FontFace, FontFallback, FontStretch, FontStyle, FontWeight, GlyphOffset,
    GlyphRunAnalysis, TextAnalysisSource, TextAnalysisSourceMethods, DWRITE_GLYPH_RUN,
    DWRITE_TEXTURE_TYPE,
};

use winapi::shared::minwindef::BOOL;
use winapi::shared::ntdef::{HRESULT, LOCALE_NAME_MAX_LENGTH};
use winapi::um::dwrite;
use winapi::um::winnls::GetUserDefaultLocaleName;
use winapi::um::winuser::{SystemParametersInfoA, SPI_GETFONTSMOOTHING};

use super::{
    BitmapBuffer, Error, FontDesc, FontKey, GlyphKey, Metrics, RasterizedGlyph, Size, Slant, Style,
    Weight,
};

/// DirectWrite uses 0 for missing glyph symbols.
/// https://docs.microsoft.com/en-us/typography/opentype/spec/recom#glyph-0-the-notdef-glyph
const MISSING_GLYPH_INDEX: u16 = 0;

/// Cached DirectWrite font.
struct Font {
    face: FontFace,
    family_name: String,
    weight: FontWeight,
    style: FontStyle,
    stretch: FontStretch,
}

/// DirectDraw texture types
#[derive(Copy, Clone)]
enum TextureType {
    Aliased1x1 = dwrote::DWRITE_TEXTURE_ALIASED_1x1 as isize,
    ClearType3x1 = dwrote::DWRITE_TEXTURE_CLEARTYPE_3x1 as isize,
}

pub struct DirectWriteRasterizer {
    fonts: HashMap<FontKey, Font>,
    keys: HashMap<FontDesc, FontKey>,
    device_pixel_ratio: f32,
    texture_type: TextureType,
    available_fonts: FontCollection,
    fallback_sequence: Option<FontFallback>,
}

impl DirectWriteRasterizer {
    fn rasterize_glyph(
        &self,
        face: &FontFace,
        size: Size,
        character: char,
        glyph_index: u16,
    ) -> Result<RasterizedGlyph, Error> {
        let em_size = em_size(size);

        let glyph_run = DWRITE_GLYPH_RUN {
            fontFace: unsafe { face.as_ptr() },
            fontEmSize: em_size,
            glyphCount: 1,
            glyphIndices: &glyph_index,
            glyphAdvances: &0.0,
            glyphOffsets: &GlyphOffset::default(),
            isSideways: 0,
            bidiLevel: 0,
        };

        let rendering_mode = match self.texture_type {
            TextureType::ClearType3x1 => face.get_recommended_rendering_mode_default_params(
                em_size,
                self.device_pixel_ratio,
                dwrote::DWRITE_MEASURING_MODE_NATURAL,
            ),
            TextureType::Aliased1x1 => dwrote::DWRITE_RENDERING_MODE_ALIASED,
        };

        let glyph_analysis = GlyphRunAnalysis::create(
            &glyph_run,
            self.device_pixel_ratio,
            None,
            rendering_mode,
            dwrote::DWRITE_MEASURING_MODE_NATURAL,
            0.0,
            0.0,
        )?;

        let bounds =
            glyph_analysis.get_alpha_texture_bounds(self.texture_type as DWRITE_TEXTURE_TYPE)?;

        let buffer = BitmapBuffer::RGB(
            self.normalize_buffer(
                glyph_analysis
                    .create_alpha_texture(self.texture_type as DWRITE_TEXTURE_TYPE, bounds)?,
            ),
        );
        Ok(RasterizedGlyph {
            character,
            width: (bounds.right - bounds.left) as i32,
            height: (bounds.bottom - bounds.top) as i32,
            top: -bounds.top,
            left: bounds.left,
            buffer,
        })
    }

    fn get_loaded_font(&self, font_key: FontKey) -> Result<&Font, Error> {
        self.fonts.get(&font_key).ok_or(Error::UnknownFontKey)
    }

    fn get_glyph_index(&self, face: &FontFace, character: char) -> u16 {
        face.get_glyph_indices(&[character as u32]).first().copied().unwrap_or(MISSING_GLYPH_INDEX)
    }

    fn get_fallback_font(&self, loaded_font: &Font, character: char) -> Option<dwrote::Font> {
        let fallback = self.fallback_sequence.as_ref()?;

        let mut buffer = [0u16; 2];
        character.encode_utf16(&mut buffer);

        let length = character.len_utf16() as u32;
        let utf16_codepoints = &buffer[..length as usize];

        let locale = get_current_locale();

        let text_analysis_source_data = TextAnalysisSourceData { locale: &locale, length };
        let text_analysis_source = TextAnalysisSource::from_text(
            Box::new(text_analysis_source_data),
            Cow::Borrowed(utf16_codepoints),
        );

        let fallback_result = fallback.map_characters(
            &text_analysis_source,
            0,
            length,
            &self.available_fonts,
            Some(&loaded_font.family_name),
            loaded_font.weight,
            loaded_font.style,
            loaded_font.stretch,
        );

        fallback_result.mapped_font
    }

    /// Given a buffer containing a rasterized glyph, return a buffer with
    /// three bytes per pixel regardless of the DirectWrite texture type in
    /// use.
    fn normalize_buffer(&self, buffer: Vec<u8>) -> Vec<u8> {
        match self.texture_type {
            TextureType::ClearType3x1 => buffer,
            TextureType::Aliased1x1 => {
                let mut norm_buf: Vec<u8> = Vec::with_capacity(buffer.len() * 3);
                for pix in buffer.iter() {
                    norm_buf.push(*pix);
                    norm_buf.push(*pix);
                    norm_buf.push(*pix);
                }
                norm_buf
            },
        }
    }
}

impl crate::Rasterize for DirectWriteRasterizer {
    fn new(device_pixel_ratio: f32, _: bool) -> Result<DirectWriteRasterizer, Error> {
        let mut use_smoothing: BOOL = 0;
        unsafe {
            SystemParametersInfoA(
                SPI_GETFONTSMOOTHING,
                0,
                &mut use_smoothing as *mut _ as *mut c_void,
                0,
            );
        }

        let texture_type = match use_smoothing {
            0 => TextureType::Aliased1x1,
            _ => TextureType::ClearType3x1,
        };

        Ok(DirectWriteRasterizer {
            fonts: HashMap::new(),
            keys: HashMap::new(),
            device_pixel_ratio,
            texture_type,
            available_fonts: FontCollection::system(),
            fallback_sequence: FontFallback::get_system_fallback(),
        })
    }

    fn metrics(&self, key: FontKey, size: Size) -> Result<Metrics, Error> {
        let face = &self.get_loaded_font(key)?.face;
        let vmetrics = face.metrics().metrics0();

        let scale = em_size(size) * self.device_pixel_ratio / f32::from(vmetrics.designUnitsPerEm);

        let underline_position = f32::from(vmetrics.underlinePosition) * scale;
        let underline_thickness = f32::from(vmetrics.underlineThickness) * scale;

        let strikeout_position = f32::from(vmetrics.strikethroughPosition) * scale;
        let strikeout_thickness = f32::from(vmetrics.strikethroughThickness) * scale;

        let ascent = f32::from(vmetrics.ascent) * scale;
        let descent = -f32::from(vmetrics.descent) * scale;
        let line_gap = f32::from(vmetrics.lineGap) * scale;

        let line_height = f64::from(ascent - descent + line_gap);

        // Since all monospace characters have the same width, we use `!` for horizontal metrics.
        let character = '!';
        let glyph_index = self.get_glyph_index(face, character);

        let glyph_metrics = face.get_design_glyph_metrics(&[glyph_index], false);
        let hmetrics = glyph_metrics.first().ok_or(Error::MetricsNotFound)?;

        let average_advance = f64::from(hmetrics.advanceWidth) * f64::from(scale);

        Ok(Metrics {
            descent,
            average_advance,
            line_height,
            underline_position,
            underline_thickness,
            strikeout_position,
            strikeout_thickness,
        })
    }

    fn load_font(&mut self, desc: &FontDesc, _size: Size) -> Result<FontKey, Error> {
        // Fast path if face is already loaded.
        if let Some(key) = self.keys.get(desc) {
            return Ok(*key);
        }

        let family = self
            .available_fonts
            .get_font_family_by_name(&desc.name)
            .ok_or_else(|| Error::FontNotFound(desc.clone()))?;

        let font = match desc.style {
            Style::Description { weight, slant } => {
                // This searches for the "best" font - should mean we don't have to worry about
                // fallbacks if our exact desired weight/style isn't available.
                Ok(family.get_first_matching_font(weight.into(), FontStretch::Normal, slant.into()))
            },
            Style::Specific(ref style) => {
                let mut idx = 0;
                let count = family.get_font_count();

                loop {
                    if idx == count {
                        break Err(Error::FontNotFound(desc.clone()));
                    }

                    let font = family.get_font(idx);

                    if font.face_name() == *style {
                        break Ok(font);
                    }

                    idx += 1;
                }
            },
        }?;

        let key = FontKey::next();
        self.keys.insert(desc.clone(), key);
        self.fonts.insert(key, font.into());

        Ok(key)
    }

    fn get_glyph(&mut self, glyph: GlyphKey) -> Result<RasterizedGlyph, Error> {
        let loaded_font = self.get_loaded_font(glyph.font_key)?;

        let loaded_fallback_font;
        let mut font = loaded_font;
        let mut glyph_index = self.get_glyph_index(&loaded_font.face, glyph.character);
        if glyph_index == MISSING_GLYPH_INDEX {
            if let Some(fallback_font) = self.get_fallback_font(&loaded_font, glyph.character) {
                loaded_fallback_font = Font::from(fallback_font);
                glyph_index = self.get_glyph_index(&loaded_fallback_font.face, glyph.character);
                font = &loaded_fallback_font;
            }
        }

        let rasterized_glyph =
            self.rasterize_glyph(&font.face, glyph.size, glyph.character, glyph_index)?;

        if glyph_index == MISSING_GLYPH_INDEX {
            Err(Error::MissingGlyph(rasterized_glyph))
        } else {
            Ok(rasterized_glyph)
        }
    }

    fn update_dpr(&mut self, device_pixel_ratio: f32) {
        self.device_pixel_ratio = device_pixel_ratio;
    }
}

fn em_size(size: Size) -> f32 {
    size.as_f32_pts() * (96.0 / 72.0)
}

impl From<dwrote::Font> for Font {
    fn from(font: dwrote::Font) -> Font {
        Font {
            face: font.create_font_face(),
            family_name: font.family_name(),
            weight: font.weight(),
            style: font.style(),
            stretch: font.stretch(),
        }
    }
}

impl From<Weight> for FontWeight {
    fn from(weight: Weight) -> FontWeight {
        match weight {
            Weight::Bold => FontWeight::Bold,
            Weight::Normal => FontWeight::Regular,
        }
    }
}

impl From<Slant> for FontStyle {
    fn from(slant: Slant) -> FontStyle {
        match slant {
            Slant::Oblique => FontStyle::Oblique,
            Slant::Italic => FontStyle::Italic,
            Slant::Normal => FontStyle::Normal,
        }
    }
}

fn get_current_locale() -> String {
    let mut buffer = vec![0u16; LOCALE_NAME_MAX_LENGTH];
    let len =
        unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), buffer.len() as i32) as usize };

    // `len` includes null byte, which we don't need in Rust.
    OsString::from_wide(&buffer[..len - 1]).into_string().expect("Locale not valid unicode")
}

/// Font fallback information for dwrote's TextAnalysisSource.
struct TextAnalysisSourceData<'a> {
    locale: &'a str,
    length: u32,
}

impl TextAnalysisSourceMethods for TextAnalysisSourceData<'_> {
    fn get_locale_name(&self, _text_position: u32) -> (Cow<str>, u32) {
        (Cow::Borrowed(self.locale), self.length)
    }

    fn get_paragraph_reading_direction(&self) -> dwrite::DWRITE_READING_DIRECTION {
        dwrite::DWRITE_READING_DIRECTION_LEFT_TO_RIGHT
    }
}

impl From<HRESULT> for Error {
    fn from(hresult: HRESULT) -> Self {
        let message = format!("a DirectWrite rendering error occurred: {:X}", hresult);
        Error::PlatformError(message)
    }
}
