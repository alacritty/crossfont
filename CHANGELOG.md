# Changelog

All notable changes to crossfont are documented in this file.
The sections should follow the order `Added`, `Changed`, `Fixed`, and `Removed`.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Fixed

- On systems using Fontconfig backend newly added fonts not visible upon `Rasterizer` recreation.

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
