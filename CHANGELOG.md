# Changelog

All notable changes to crossfont are documented in this file.
The sections should follow the order `Added`, `Changed`, `Fixed`, and `Removed`.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

## 0.8.1

### Changed

- MSRV changed to 1.71.0
- Fixed leak after searching a font with fontconfig backend

## 0.8.0

### Changed

- **Breaking** fontconfig system library provider changed to yeslogic-fontcontconfig-sys
- **Breaking** freetype-rs bumped to 0.36.0

### Fixed

- On macOS, `AppleFontSmoothing` not recognized when specified as string

## 0.7.0

### Changed

- `Size::as_px` and `Size::from_px` now use `f32` type

## 0.6.0

### Changed

- `Size` now uses 6 floating point digits precision instead of rounding to 0.5
- Add `Size::from_px`, `Size::as_px`, `Size::as_pt`, and `Size::scale`
- Remove `Rasterizer::update_dpr`; users should scale fonts themselves

## 0.5.2

- Minimum Rust version has been bumped to 1.65

## 0.5.1

### Fixed

- Font size of scalable colored glyphs
- macOS underline metrics being relative to descent and not baseline

## 0.5.0

### Added

- On macOS, use the `AppleFontSmoothing` user default to decide whether fonts should be "smoothed"

### Changed

- Renamed `darwin::Rasterizer` to `darwin::CoreTextRasterizer`

### Fixed

- On macOS, `use_thin_strokes` and `set_font_smoothing` did not work since Big Sur

### Removed

- `use_thin_strokes` parameter from `Rasterize::new` trait method
- `set_font_smoothing` from the `darwin` module
- `get_family_names` from the `darwin` module

## 0.4.2

### Fixed

- Crash on macOS when loading disabled font

## 0.4.1

### Fixed

- Fix 32-bit build with FreeType/Fontconfig backend

## 0.4.0

### Added

- FreeType proportional font metrics using `RasterizedGlyph::advance` and `Rasterize::kerning`

### Changed

- Minimum Rust version has been bumped to 1.56.0

## 0.3.2

### Changed

- Minimum Rust version has been bumped to 1.46.0
- Core Text backend uses a current font as the original fallback font instead of Menlo

### Fixed

- Core Text backend ignoring style for font fallback

## 0.3.1

### Fixed

- Fontconfig not checking for fonts installed after `Rasterizer` creation
- Crash with non-utf8 font paths on Linux
- Bitmap rendering with FreeType 2.11.0

## 0.3.0

### Changed

- FreeType font height metric will now use `(ascent - descent)` if it is bigger than height
- Several types have been renamed to comply with the `upper_case_acronyms` clippy lint

## 0.2.0

### Changed

- The rasterizer's `Error` type is now shared across platforms
- Missing glyphs are now returned as the content of the `MissingGlyph` error
- `RasterizedGlyph`'s `c` and `buf` fields are now named `character` and `buffer` respectively
- `GlyphKey`'s `c` field is now named `character`

## 0.1.1

### Changed

- Minimum Rust version has been bumped to 1.43.0

### Fixed

- Compilation with FreeType version below 2.8.0 on Linux/BSD
