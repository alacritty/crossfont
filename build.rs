fn main() {
    // This libtool version maps to FreeType version 2.8.0, so we can use
    // `FT_Set_Default_Properties`.
    #[cfg(not(any(target_os = "macos", windows)))]
    if pkg_config::Config::new().atleast_version("20.0.14").probe("freetype2").is_ok() {
        println!("cargo:rustc-cfg=ft_set_default_properties_available")
    }
}
