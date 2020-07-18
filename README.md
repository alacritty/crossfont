# crossfont

crossfont is a cross-platform Rust library for loading fonts and rasterizing
glyphs, using native font engines whenever possible.

### Supported Backends

| Platform | Backends    |
|----------|-------------|
| Linux    | Freetype    |
| BSD      | Freetype    |
| Windows  | DirectWrite |
| macOS    | Core Text   |

### Known Issues

Since crossfont was originally made solely for rendering monospace fonts in
[Alacritty](https://github.com/alacritty/alacritty), there currently is only
very limited support for proportional fonts.

Loading a lot of different fonts might also lead to resource leakage since they
are not explicitly dropped from the cache.
