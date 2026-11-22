//! The MSL source of the shaders, for the shader hot swap (W36).
//!
//! `nxb build` with `NXB_KEEP_MSL=1` copies the `<key><suffix>.metal` files of the shader step to
//! `<run>/shader-src` beside `<run>/shaders`. The host reads them for `shader dump`. A normal build leaves them out,
//! because they about double the size of the shader folder.

/// The folder of the sources in the run folder.
pub const DIR: &str = "shader-src";

/// The build key that keeps the sources.
pub const KEEP_ENV: &str = "NXB_KEEP_MSL";

/// The file name of the source of the library `<key><suffix>` (`suffix` is `_b` or `_bp`).
pub fn file_name(key: &str, suffix: &str) -> String {
    format!("{key}{suffix}.metal")
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_file_name_is_key_and_suffix() {
        assert_eq!(super::file_name("00ab", "_bp"), "00ab_bp.metal");
    }
}
