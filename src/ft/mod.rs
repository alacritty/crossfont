//! Rasterization powered by FreeType and Fontconfig.

use std::cmp::{min, Ordering};
use std::collections::HashMap;
use std::fmt::{self, Formatter};
use std::rc::Rc;
use std::time::{Duration, Instant};

use freetype::face::LoadFlag;
use freetype::tt_os2::TrueTypeOS2Table;
use freetype::{self, Library, Matrix};
use freetype::{freetype_sys, Face as FtFace};
use libc::{c_long, c_uint};
use log::{debug, trace};

pub mod fc;

use fc::{CharSet, FtFaceLocation, Pattern, PatternHash, PatternRef, Rgba};

use super::{
    BitmapBuffer, Error, FontDesc, FontKey, GlyphKey, Metrics, Rasterize, RasterizedGlyph, Size,
    Slant, Style, Weight,
};

/// FreeType uses 0 for the missing glyph:
/// https://freetype.org/freetype2/docs/reference/ft2-base_interface.html#ft_get_char_index
const MISSING_GLYPH_INDEX: u32 = 0;

/// Delay before font config reload after creating the `Rasterizer`.
const RELOAD_DELAY: Duration = Duration::from_secs(2);

struct FallbackFont {
    pattern: Pattern,
    key: FontKey,
}

impl FallbackFont {
    fn new(pattern: Pattern, key: FontKey) -> FallbackFont {
        Self { pattern, key }
    }
}

impl FontKey {
    fn from_pattern_hashes(lhs: PatternHash, rhs: PatternHash) -> Self {
        // XOR two hashes to get a font ID.
        Self { token: lhs.0.rotate_left(1) ^ rhs.0 }
    }
}

#[derive(Default)]
struct FallbackList {
    list: Vec<FallbackFont>,
    coverage: CharSet,
}

struct FaceLoadingProperties {
    load_flags: LoadFlag,
    render_mode: freetype::RenderMode,
    lcd_filter: c_uint,
    non_scalable: Option<f32>,
    colored_bitmap: bool,
    embolden: bool,
    matrix: Option<Matrix>,
    pixelsize_fixup_factor: Option<f64>,
    ft_face: Rc<FtFace>,
    rgba: Rgba,
}

impl fmt::Debug for FaceLoadingProperties {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Face")
            .field("ft_face", &self.ft_face)
            .field("load_flags", &self.load_flags)
            .field("render_mode", &match self.render_mode {
                freetype::RenderMode::Normal => "Normal",
                freetype::RenderMode::Light => "Light",
                freetype::RenderMode::Mono => "Mono",
                freetype::RenderMode::Lcd => "Lcd",
                freetype::RenderMode::LcdV => "LcdV",
                freetype::RenderMode::Max => "Max",
            })
            .field("lcd_filter", &self.lcd_filter)
            .finish()
    }
}

/// Rasterizes glyphs for a single font face.
pub struct FreeTypeRasterizer {
    loader: FreeTypeLoader,
    fallback_lists: HashMap<FontKey, FallbackList>,
    device_pixel_ratio: f32,

    /// Rasterizer creation time stamp to delay lazy font config updates
    /// in `Rasterizer::load_font`.
    creation_timestamp: Option<Instant>,
}

#[inline]
fn to_freetype_26_6(f: f32) -> isize {
    ((1i32 << 6) as f32 * f).round() as isize
}

#[inline]
fn to_fixedpoint_16_6(f: f64) -> c_long {
    (f * 65536.0) as c_long
}

#[inline]
fn from_freetype_26_6(f: impl IntoF32) -> f32 {
    f.into_f32() / 64.
}

trait IntoF32 {
    fn into_f32(self) -> f32;
}

impl IntoF32 for f32 {
    fn into_f32(self) -> f32 {
        self
    }
}

impl IntoF32 for i32 {
    fn into_f32(self) -> f32 {
        self as f32
    }
}

impl IntoF32 for i64 {
    fn into_f32(self) -> f32 {
        self as f32
    }
}

impl Rasterize for FreeTypeRasterizer {
    fn new(device_pixel_ratio: f32) -> Result<FreeTypeRasterizer, Error> {
        Ok(FreeTypeRasterizer {
            loader: FreeTypeLoader::new()?,
            fallback_lists: HashMap::new(),
            device_pixel_ratio,
            creation_timestamp: Some(Instant::now()),
        })
    }

    fn metrics(&self, key: FontKey, _size: Size) -> Result<Metrics, Error> {
        let face = &mut self.loader.faces.get(&key).ok_or(Error::UnknownFontKey)?;
        let full = self.full_metrics(face)?;

        let ascent = from_freetype_26_6(full.size_metrics.ascender);
        let descent = from_freetype_26_6(full.size_metrics.descender);
        let glyph_height = from_freetype_26_6(full.size_metrics.height) as f64;
        let global_glyph_height = (ascent - descent) as f64;
        let height = f64::max(glyph_height, global_glyph_height);

        // Get underline position and thickness in device pixels.
        let x_scale = full.size_metrics.x_scale as f32 / 65536.0;
        let ft_underline_position = face.ft_face.underline_position();
        let mut underline_position = from_freetype_26_6(ft_underline_position as f32 * x_scale);
        let ft_underline_thickness = face.ft_face.underline_thickness();
        let mut underline_thickness = from_freetype_26_6(ft_underline_thickness as f32 * x_scale);

        // Fallback for bitmap fonts which do not provide underline metrics.
        if underline_position == 0. {
            underline_thickness = (descent.abs() / 5.).round();
            underline_position = descent / 2.;
        }

        // Get strikeout position and thickness in device pixels.
        let (strikeout_position, strikeout_thickness) =
            match TrueTypeOS2Table::from_face(&mut (*face.ft_face).clone()) {
                Some(os2) => (
                    from_freetype_26_6(os2.y_strikeout_position() as f32 * x_scale),
                    from_freetype_26_6(os2.y_strikeout_size() as f32 * x_scale),
                ),
                _ => {
                    // Fallback if font doesn't provide info about strikeout.
                    trace!("Using fallback strikeout metrics");
                    let strikeout_position = height as f32 / 2. + descent;
                    (strikeout_position, underline_thickness)
                },
            };

        Ok(Metrics {
            average_advance: full.cell_width,
            line_height: height,
            descent,
            underline_position,
            underline_thickness,
            strikeout_position,
            strikeout_thickness,
        })
    }

    fn load_font(&mut self, desc: &FontDesc, size: Size) -> Result<FontKey, Error> {
        if self.creation_timestamp.map_or(true, |timestamp| timestamp.elapsed() > RELOAD_DELAY) {
            self.creation_timestamp = None;
            fc::update_config();
        }

        self.get_face(desc, size)
    }

    fn get_glyph(&mut self, glyph_key: GlyphKey) -> Result<RasterizedGlyph, Error> {
        let font_key = self.face_for_glyph(glyph_key);
        let face = &self.loader.faces[&font_key];
        let index = face.ft_face.get_char_index(glyph_key.character as usize);
        let pixelsize = face
            .non_scalable
            .unwrap_or_else(|| glyph_key.size.as_f32_pts() * self.device_pixel_ratio * 96. / 72.);

        if !face.colored_bitmap {
            face.ft_face.set_char_size(to_freetype_26_6(pixelsize), 0, 0, 0)?;
        }

        unsafe {
            let ft_lib = self.loader.library.raw();
            freetype::ffi::FT_Library_SetLcdFilter(ft_lib, face.lcd_filter);
        }

        face.ft_face.load_glyph(index, face.load_flags)?;

        let glyph = face.ft_face.glyph();

        // Generate synthetic bold.
        if face.embolden {
            unsafe {
                freetype_sys::FT_GlyphSlot_Embolden(glyph.raw()
                    as *const freetype_sys::FT_GlyphSlotRec
                    as *mut freetype_sys::FT_GlyphSlotRec);
            }
        }

        let advance = unsafe {
            // Transform glyphs with the matrix from Fontconfig. Primarily used to generate italics.
            let raw_glyph = face.ft_face.raw().glyph;
            if let Some(matrix) = face.matrix.as_ref() {
                // Check that the glyph is a vectorial outline, not a bitmap.
                if (*raw_glyph).format == freetype_sys::FT_GLYPH_FORMAT_OUTLINE {
                    let outline = &(*raw_glyph).outline;

                    freetype_sys::FT_Outline_Transform(outline, matrix);
                }
            }

            // Don't render bitmap glyphs, it results in error with freestype 2.11.0.
            if (*raw_glyph).format != freetype_sys::FT_GLYPH_FORMAT_BITMAP {
                glyph.render_glyph(face.render_mode)?;
            }

            let advance = (*raw_glyph).advance;
            (from_freetype_26_6(advance.x) as i32, from_freetype_26_6(advance.y) as i32)
        };

        let (pixel_height, pixel_width, buffer) =
            Self::normalize_buffer(&glyph.bitmap(), &face.rgba)?;

        let mut rasterized_glyph = RasterizedGlyph {
            character: glyph_key.character,
            top: glyph.bitmap_top(),
            left: glyph.bitmap_left(),
            width: pixel_width,
            height: pixel_height,
            advance,
            buffer,
        };

        if index == MISSING_GLYPH_INDEX {
            return Err(Error::MissingGlyph(rasterized_glyph));
        }

        if face.colored_bitmap {
            let fixup_factor = match face.pixelsize_fixup_factor {
                Some(fixup_factor) => fixup_factor,
                None => {
                    // Fallback if the user has bitmap scaling disabled.
                    let metrics = face.ft_face.size_metrics().ok_or(Error::MetricsNotFound)?;
                    f64::from(pixelsize) / f64::from(metrics.y_ppem)
                },
            };

            // Scale glyph advance.
            rasterized_glyph.advance.0 = (advance.0 as f64 * fixup_factor).round() as i32;
            rasterized_glyph.advance.1 = (advance.1 as f64 * fixup_factor).round() as i32;

            rasterized_glyph = downsample_bitmap(rasterized_glyph, fixup_factor);
        }

        Ok(rasterized_glyph)
    }

    fn kerning(&mut self, left: GlyphKey, right: GlyphKey) -> (f32, f32) {
        let font_key = self.face_for_glyph(left);
        let mut ft_face = (*self.loader.faces[&font_key].ft_face).clone();

        if !freetype_sys::FT_HAS_KERNING(ft_face.raw_mut()) {
            return (0., 0.);
        }

        let left = ft_face.get_char_index(left.character as usize);
        let right = ft_face.get_char_index(right.character as usize);

        let mut kerning = freetype_sys::FT_Vector::default();
        let mode = freetype_sys::FT_KERNING_DEFAULT;

        unsafe {
            freetype_sys::FT_Get_Kerning(ft_face.raw_mut(), left, right, mode, &mut kerning);
        }

        (from_freetype_26_6(kerning.x), from_freetype_26_6(kerning.y))
    }

    fn update_dpr(&mut self, device_pixel_ratio: f32) {
        self.device_pixel_ratio = device_pixel_ratio;
    }
}

impl From<Slant> for fc::Slant {
    fn from(slant: Slant) -> Self {
        match slant {
            Slant::Normal => fc::Slant::Roman,
            Slant::Italic => fc::Slant::Italic,
            Slant::Oblique => fc::Slant::Oblique,
        }
    }
}

impl From<Weight> for fc::Weight {
    fn from(weight: Weight) -> Self {
        match weight {
            Weight::Normal => fc::Weight::Regular,
            Weight::Bold => fc::Weight::Bold,
        }
    }
}

struct FullMetrics {
    size_metrics: freetype::ffi::FT_Size_Metrics,
    cell_width: f64,
}

impl FreeTypeRasterizer {
    /// Load a font face according to `FontDesc`.
    fn get_face(&mut self, desc: &FontDesc, size: Size) -> Result<FontKey, Error> {
        // Adjust for DPR.
        let size = f64::from(size.as_f32_pts() * self.device_pixel_ratio * 96. / 72.);

        let config = fc::Config::get_current();
        let mut pattern = Pattern::new();
        pattern.add_family(&desc.name);
        pattern.add_pixelsize(size);

        // Add style to a pattern.
        match desc.style {
            Style::Description { slant, weight } => {
                // Match nearest font.
                pattern.set_weight(weight.into());
                pattern.set_slant(slant.into());
            },
            Style::Specific(ref style) => {
                // If a name was specified, try and load specifically that font.
                pattern.add_style(style);
            },
        }

        // Hash requested pattern.
        let hash = pattern.hash();

        pattern.config_substitute(config, fc::MatchKind::Pattern);
        pattern.default_substitute();

        // Get font list using pattern. First font is the primary one while the rest are fallbacks.
        let matched_fonts =
            fc::font_sort(config, &pattern).ok_or_else(|| Error::FontNotFound(desc.to_owned()))?;
        let mut matched_fonts = matched_fonts.into_iter();

        let primary_font =
            matched_fonts.next().ok_or_else(|| Error::FontNotFound(desc.to_owned()))?;

        // We should render patterns to get values like `pixelsizefixupfactor`.
        let primary_font = pattern.render_prepare(config, primary_font);

        // Hash pattern together with request pattern to include requested font size in the hash.
        let primary_font_key = FontKey::from_pattern_hashes(hash, primary_font.hash());

        // Return if we already have the same primary font.
        if self.fallback_lists.contains_key(&primary_font_key) {
            return Ok(primary_font_key);
        }

        // Load font if we haven't loaded it yet.
        if !self.loader.faces.contains_key(&primary_font_key) {
            self.loader
                .face_from_pattern(&primary_font, primary_font_key)
                .and_then(|pattern| pattern.ok_or_else(|| Error::FontNotFound(desc.to_owned())))?;
        }

        // Coverage for fallback fonts.
        let coverage = CharSet::new();
        let empty_charset = CharSet::new();

        let list: Vec<FallbackFont> = matched_fonts
            .map(|fallback_font| {
                let charset = fallback_font.get_charset().unwrap_or(&empty_charset);

                // Use original pattern to preserve loading flags.
                let fallback_font = pattern.render_prepare(config, fallback_font);
                let fallback_font_key = FontKey::from_pattern_hashes(hash, fallback_font.hash());

                coverage.merge(charset);

                FallbackFont::new(fallback_font, fallback_font_key)
            })
            .collect();

        self.fallback_lists.insert(primary_font_key, FallbackList { list, coverage });

        Ok(primary_font_key)
    }

    fn full_metrics(&self, face_load_props: &FaceLoadingProperties) -> Result<FullMetrics, Error> {
        let ft_face = &face_load_props.ft_face;
        let size_metrics = ft_face.size_metrics().ok_or(Error::MetricsNotFound)?;

        let width = match ft_face.load_char('0' as usize, face_load_props.load_flags) {
            Ok(_) => from_freetype_26_6(ft_face.glyph().metrics().horiAdvance),
            Err(_) => from_freetype_26_6(size_metrics.max_advance),
        };

        Ok(FullMetrics { size_metrics, cell_width: width as f64 })
    }

    fn face_for_glyph(&mut self, glyph_key: GlyphKey) -> FontKey {
        if let Some(face) = self.loader.faces.get(&glyph_key.font_key) {
            let index = face.ft_face.get_char_index(glyph_key.character as usize);

            if index != 0 {
                return glyph_key.font_key;
            }
        }

        self.load_face_with_glyph(glyph_key).unwrap_or(glyph_key.font_key)
    }

    fn load_face_with_glyph(&mut self, glyph: GlyphKey) -> Result<FontKey, Error> {
        let fallback_list = self.fallback_lists.get(&glyph.font_key).unwrap();

        // Check whether glyph is presented in any fallback font.
        if !fallback_list.coverage.has_char(glyph.character) {
            return Ok(glyph.font_key);
        }

        for fallback_font in &fallback_list.list {
            let font_key = fallback_font.key;
            let font_pattern = &fallback_font.pattern;
            match self.loader.faces.get(&font_key) {
                Some(face) => {
                    let index = face.ft_face.get_char_index(glyph.character as usize);

                    // We found something in a current face, so let's use it.
                    if index != 0 {
                        return Ok(font_key);
                    }
                },
                None => {
                    if !font_pattern.get_charset().map_or(false, |cs| cs.has_char(glyph.character))
                    {
                        continue;
                    }

                    let pattern = font_pattern.clone();
                    if let Some(key) = self.loader.face_from_pattern(&pattern, font_key)? {
                        return Ok(key);
                    }
                },
            }
        }

        // You can hit this return, if you're failing to get charset from a pattern.
        Ok(glyph.font_key)
    }

    /// Given a FreeType `Bitmap`, returns packed buffer with 1 byte per LCD channel.
    ///
    /// The i32 value in the return type is the number of pixels per row.
    fn normalize_buffer(
        bitmap: &freetype::bitmap::Bitmap,
        rgba: &Rgba,
    ) -> freetype::FtResult<(i32, i32, BitmapBuffer)> {
        use freetype::bitmap::PixelMode;

        let buf = bitmap.buffer();
        let mut packed = Vec::with_capacity((bitmap.rows() * bitmap.width()) as usize);
        let pitch = bitmap.pitch().unsigned_abs() as usize;
        match bitmap.pixel_mode()? {
            PixelMode::Lcd => {
                for i in 0..bitmap.rows() {
                    let start = (i as usize) * pitch;
                    let stop = start + bitmap.width() as usize;
                    match rgba {
                        Rgba::Bgr => {
                            for j in (start..stop).step_by(3) {
                                packed.push(buf[j + 2]);
                                packed.push(buf[j + 1]);
                                packed.push(buf[j]);
                            }
                        },
                        _ => packed.extend_from_slice(&buf[start..stop]),
                    }
                }
                Ok((bitmap.rows(), bitmap.width() / 3, BitmapBuffer::Rgb(packed)))
            },
            PixelMode::LcdV => {
                for i in 0..bitmap.rows() / 3 {
                    for j in 0..bitmap.width() {
                        for k in 0..3 {
                            let k = match rgba {
                                Rgba::Vbgr => 2 - k,
                                _ => k,
                            };
                            let offset = ((i as usize) * 3 + k) * pitch + (j as usize);
                            packed.push(buf[offset]);
                        }
                    }
                }
                Ok((bitmap.rows() / 3, bitmap.width(), BitmapBuffer::Rgb(packed)))
            },
            // Mono data is stored in a packed format using 1 bit per pixel.
            PixelMode::Mono => {
                fn unpack_byte(res: &mut Vec<u8>, byte: u8, mut count: u8) {
                    // Mono stores MSBit at top of byte
                    let mut bit = 7;
                    while count != 0 {
                        let value = ((byte >> bit) & 1) * 255;
                        // Push value 3x since result buffer should be 1 byte
                        // per channel.
                        res.push(value);
                        res.push(value);
                        res.push(value);
                        count -= 1;
                        bit -= 1;
                    }
                }

                for i in 0..(bitmap.rows() as usize) {
                    let mut columns = bitmap.width();
                    let mut byte = 0;
                    let offset = i * bitmap.pitch().unsigned_abs() as usize;
                    while columns != 0 {
                        let bits = min(8, columns);
                        unpack_byte(&mut packed, buf[offset + byte], bits as u8);

                        columns -= bits;
                        byte += 1;
                    }
                }
                Ok((bitmap.rows(), bitmap.width(), BitmapBuffer::Rgb(packed)))
            },
            // Gray data is stored as a value between 0 and 255 using 1 byte per pixel.
            PixelMode::Gray => {
                for i in 0..bitmap.rows() {
                    let start = (i as usize) * pitch;
                    let stop = start + bitmap.width() as usize;
                    for byte in &buf[start..stop] {
                        packed.push(*byte);
                        packed.push(*byte);
                        packed.push(*byte);
                    }
                }
                Ok((bitmap.rows(), bitmap.width(), BitmapBuffer::Rgb(packed)))
            },
            PixelMode::Bgra => {
                let buf_size = (bitmap.rows() * bitmap.width() * 4) as usize;
                let mut i = 0;
                while i < buf_size {
                    packed.push(buf[i + 2]);
                    packed.push(buf[i + 1]);
                    packed.push(buf[i]);
                    packed.push(buf[i + 3]);
                    i += 4;
                }
                Ok((bitmap.rows(), bitmap.width(), BitmapBuffer::Rgba(packed)))
            },
            mode => panic!("unhandled pixel mode: {:?}", mode),
        }
    }
}

/// Downscale a bitmap by a fixed factor.
///
/// This will take the `bitmap_glyph` as input and return the glyph's content downscaled by
/// `fixup_factor`.
fn downsample_bitmap(mut bitmap_glyph: RasterizedGlyph, fixup_factor: f64) -> RasterizedGlyph {
    // Only scale colored buffers which are bigger than required.
    let bitmap_buffer = match (&bitmap_glyph.buffer, fixup_factor.partial_cmp(&1.0)) {
        (BitmapBuffer::Rgba(buffer), Some(Ordering::Less)) => buffer,
        _ => return bitmap_glyph,
    };

    let bitmap_width = bitmap_glyph.width as usize;
    let bitmap_height = bitmap_glyph.height as usize;

    let target_width = (bitmap_width as f64 * fixup_factor) as usize;
    let target_height = (bitmap_height as f64 * fixup_factor) as usize;

    // Number of pixels in the input buffer, per pixel in the output buffer.
    let downsampling_step = 1.0 / fixup_factor;

    let mut downsampled_buffer = Vec::<u8>::with_capacity(target_width * target_height * 4);

    for line_index in 0..target_height {
        // Get the first and last line which will be consolidated in the current output pixel.
        let line_index = line_index as f64;
        let source_line_start = (line_index * downsampling_step).round() as usize;
        let source_line_end = ((line_index + 1.) * downsampling_step).round() as usize;

        for column_index in 0..target_width {
            // Get the first and last column which will be consolidated in the current output
            // pixel.
            let column_index = column_index as f64;
            let source_column_start = (column_index * downsampling_step).round() as usize;
            let source_column_end = ((column_index + 1.) * downsampling_step).round() as usize;

            let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
            let mut pixels_picked: u32 = 0;

            // Consolidate all pixels within the source rectangle into a single averaged pixel.
            for source_line in source_line_start..source_line_end {
                let source_pixel_index = source_line * bitmap_width;

                for source_column in source_column_start..source_column_end {
                    let offset = (source_pixel_index + source_column) * 4;
                    r += u32::from(bitmap_buffer[offset]);
                    g += u32::from(bitmap_buffer[offset + 1]);
                    b += u32::from(bitmap_buffer[offset + 2]);
                    a += u32::from(bitmap_buffer[offset + 3]);
                    pixels_picked += 1;
                }
            }

            // Add a single pixel to the output buffer for the downscaled source rectangle.
            downsampled_buffer.push((r / pixels_picked) as u8);
            downsampled_buffer.push((g / pixels_picked) as u8);
            downsampled_buffer.push((b / pixels_picked) as u8);
            downsampled_buffer.push((a / pixels_picked) as u8);
        }
    }

    bitmap_glyph.buffer = BitmapBuffer::Rgba(downsampled_buffer);

    // Downscale the metrics.
    bitmap_glyph.top = (f64::from(bitmap_glyph.top) * fixup_factor) as i32;
    bitmap_glyph.left = (f64::from(bitmap_glyph.left) * fixup_factor) as i32;
    bitmap_glyph.width = target_width as i32;
    bitmap_glyph.height = target_height as i32;

    bitmap_glyph
}

impl From<freetype::Error> for Error {
    fn from(val: freetype::Error) -> Error {
        Error::PlatformError(val.to_string())
    }
}

unsafe impl Send for FreeTypeRasterizer {}

struct FreeTypeLoader {
    library: Library,
    faces: HashMap<FontKey, FaceLoadingProperties>,
    ft_faces: HashMap<FtFaceLocation, Rc<FtFace>>,
}

impl FreeTypeLoader {
    fn new() -> Result<FreeTypeLoader, Error> {
        let library = Library::init()?;

        #[cfg(ft_set_default_properties_available)]
        unsafe {
            // Initialize default properties, like user preferred interpreter.
            freetype_sys::FT_Set_Default_Properties(library.raw());
        };

        Ok(FreeTypeLoader { library, faces: HashMap::new(), ft_faces: HashMap::new() })
    }

    fn load_ft_face(&mut self, ft_face_location: FtFaceLocation) -> Result<Rc<FtFace>, Error> {
        let mut ft_face = self.library.new_face(&ft_face_location.path, ft_face_location.index)?;
        if ft_face.has_color() && !ft_face.is_scalable() {
            unsafe {
                // Select the colored bitmap size to use from the array of available sizes.
                freetype_sys::FT_Select_Size(ft_face.raw_mut(), 0);
            }
        }

        let ft_face = Rc::new(ft_face);
        self.ft_faces.insert(ft_face_location, Rc::clone(&ft_face));

        Ok(ft_face)
    }

    fn face_from_pattern(
        &mut self,
        pattern: &PatternRef,
        font_key: FontKey,
    ) -> Result<Option<FontKey>, Error> {
        if let Some(ft_face_location) = pattern.ft_face_location(0) {
            if self.faces.get(&font_key).is_some() {
                return Ok(Some(font_key));
            }

            trace!("Got font path={:?}, index={:?}", ft_face_location.path, ft_face_location.index);

            let ft_face = match self.ft_faces.get(&ft_face_location) {
                Some(ft_face) => Rc::clone(ft_face),
                None => self.load_ft_face(ft_face_location)?,
            };

            let non_scalable = if pattern.scalable().next().unwrap_or(true) {
                None
            } else {
                Some(pattern.pixelsize().next().expect("has 1+ pixelsize") as f32)
            };

            let embolden = pattern.embolden().next().unwrap_or(false);

            let matrix = pattern.get_matrix().map(|matrix| {
                // Convert Fontconfig matrix to FreeType matrix.
                let xx = to_fixedpoint_16_6(matrix.xx);
                let xy = to_fixedpoint_16_6(matrix.xy);
                let yx = to_fixedpoint_16_6(matrix.yx);
                let yy = to_fixedpoint_16_6(matrix.yy);

                Matrix { xx, xy, yx, yy }
            });

            let pixelsize_fixup_factor = pattern.pixelsizefixupfactor().next();

            let rgba = pattern.rgba().next().unwrap_or(Rgba::Unknown);

            let face = FaceLoadingProperties {
                load_flags: Self::ft_load_flags(pattern),
                render_mode: Self::ft_render_mode(pattern),
                lcd_filter: Self::ft_lcd_filter(pattern),
                non_scalable,
                colored_bitmap: ft_face.has_color() && !ft_face.is_scalable(),
                embolden,
                matrix,
                pixelsize_fixup_factor,
                ft_face,
                rgba,
            };

            debug!("Loaded Face {:?}", face);

            self.faces.insert(font_key, face);

            Ok(Some(font_key))
        } else {
            Ok(None)
        }
    }

    fn ft_load_flags(pattern: &PatternRef) -> LoadFlag {
        let antialias = pattern.antialias().next().unwrap_or(true);
        let autohint = pattern.autohint().next().unwrap_or(false);
        let hinting = pattern.hinting().next().unwrap_or(true);
        let rgba = pattern.rgba().next().unwrap_or(Rgba::Unknown);
        let embedded_bitmaps = pattern.embeddedbitmap().next().unwrap_or(true);
        let scalable = pattern.scalable().next().unwrap_or(true);
        let color = pattern.color().next().unwrap_or(false);

        // Disable hinting if so was requested.
        let hintstyle = if hinting {
            pattern.hintstyle().next().unwrap_or(fc::HintStyle::Full)
        } else {
            fc::HintStyle::None
        };

        let mut flags = match (antialias, hintstyle, rgba) {
            (false, fc::HintStyle::None, _) => LoadFlag::NO_HINTING | LoadFlag::MONOCHROME,
            (false, ..) => LoadFlag::TARGET_MONO | LoadFlag::MONOCHROME,
            (true, fc::HintStyle::None, _) => LoadFlag::NO_HINTING,
            // `hintslight` does *not* use LCD hinting even when a subpixel mode
            // is selected.
            //
            // According to the FreeType docs,
            //
            // > You can use a hinting algorithm that doesn't correspond to the
            // > same rendering mode.  As an example, it is possible to use the
            // > ‘light’ hinting algorithm and have the results rendered in
            // > horizontal LCD pixel mode.
            //
            // In practice, this means we can have `FT_LOAD_TARGET_LIGHT` with
            // subpixel render modes like `FT_RENDER_MODE_LCD`. Libraries like
            // cairo take the same approach and consider `hintslight` to always
            // prefer `FT_LOAD_TARGET_LIGHT`.
            (true, fc::HintStyle::Slight, _) => LoadFlag::TARGET_LIGHT,
            (true, fc::HintStyle::Medium, _) => LoadFlag::TARGET_NORMAL,
            // If LCD hinting is to be used, must select hintmedium or hintfull,
            // have AA enabled, and select a subpixel mode.
            (true, fc::HintStyle::Full, Rgba::Rgb) | (true, fc::HintStyle::Full, Rgba::Bgr) => {
                LoadFlag::TARGET_LCD
            },
            (true, fc::HintStyle::Full, Rgba::Vrgb) | (true, fc::HintStyle::Full, Rgba::Vbgr) => {
                LoadFlag::TARGET_LCD_V
            },
            // For non-rgba modes with Full hinting, just use the default hinting algorithm.
            (true, fc::HintStyle::Full, Rgba::Unknown)
            | (true, fc::HintStyle::Full, Rgba::None) => LoadFlag::TARGET_NORMAL,
        };

        // Non scalable fonts only have bitmaps, so disabling them entirely is likely not a
        // desirable thing. Colored fonts aren't scalable, but also only have bitmaps.
        if !embedded_bitmaps && scalable && !color {
            flags |= LoadFlag::NO_BITMAP;
        }

        // Use color for colored fonts.
        if color {
            flags |= LoadFlag::COLOR;
        }

        // Force autohint if it was requested.
        if autohint {
            flags |= LoadFlag::FORCE_AUTOHINT;
        }

        flags
    }

    fn ft_render_mode(pat: &PatternRef) -> freetype::RenderMode {
        let antialias = pat.antialias().next().unwrap_or(true);
        let rgba = pat.rgba().next().unwrap_or(Rgba::Unknown);

        match (antialias, rgba) {
            (false, _) => freetype::RenderMode::Mono,
            (_, Rgba::Rgb) | (_, Rgba::Bgr) => freetype::RenderMode::Lcd,
            (_, Rgba::Vrgb) | (_, Rgba::Vbgr) => freetype::RenderMode::LcdV,
            (true, _) => freetype::RenderMode::Normal,
        }
    }

    fn ft_lcd_filter(pat: &PatternRef) -> c_uint {
        match pat.lcdfilter().next().unwrap_or(fc::LcdFilter::Default) {
            fc::LcdFilter::None => freetype::ffi::FT_LCD_FILTER_NONE,
            fc::LcdFilter::Default => freetype::ffi::FT_LCD_FILTER_DEFAULT,
            fc::LcdFilter::Light => freetype::ffi::FT_LCD_FILTER_LIGHT,
            fc::LcdFilter::Legacy => freetype::ffi::FT_LCD_FILTER_LEGACY,
        }
    }
}
