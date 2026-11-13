//! Compile conventions per target (design-v2 9.2): the flags of Apple's compiler and the record string that names
//! them in the reflection JSON and the Metal cache key. In V: a change here changes output identity.

use nxb_model::Target;

/// How shaders of one target are compiled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TargetConv {
    /// The target.
    pub target: Target,
    /// `metal` flags before the source, `-c` included (as Python's `MAC_METAL` after `xcrun -sdk <sdk> metal`), and
    /// `BRACKET_DEPTH` last.
    pub compile_flags: &'static [&'static str],
}

/// More nested brackets than the front end's default of 256 (`METAL_DEPTH` of `dmsh_build.py`, `DEPTH` of
/// `ios_metal.py`). A source within the default limit compiles to the same bytes with it, so the record does not name it.
pub const BRACKET_DEPTH: &str = "-fbracket-depth=1024";

/// The Mac convention: `metal -c -O1 -fmetal-math-mode=relaxed -fpreserve-invariance -fwrapv`, then `metallib` (RM 3.3).
/// Relaxed math keeps NaN and Inf: the shader code of the console relies on them (`0 x Inf`, the `isnan` guard of a
/// texture coordinate), and fast math may fold both away.
pub const MAC: TargetConv = TargetConv {
    target: Target::Mac,
    compile_flags: &[
        "-c",
        "-O1",
        "-fmetal-math-mode=relaxed",
        "-fpreserve-invariance",
        "-fwrapv",
        BRACKET_DEPTH,
    ],
};

/// The iOS convention (`dm/ios/tools/ios_metal.py`): iOS 18, relaxed math (NaN and Inf kept), invariance, `-O1`.
pub const IOS: TargetConv = TargetConv {
    target: Target::Ios,
    compile_flags: &[
        "-c",
        "-O1",
        "-mios-version-min=18.0",
        "-fmetal-math-mode=relaxed",
        "-fpreserve-invariance",
        "-fwrapv",
        BRACKET_DEPTH,
    ],
};

/// iOS 26 convention for the standalone IPA host.
pub const IOS26: TargetConv = TargetConv {
    target: Target::Ios,
    compile_flags: &[
        "-c",
        "-O1",
        "-mios-version-min=26.0",
        "-fmetal-math-mode=relaxed",
        "-fpreserve-invariance",
        "-fwrapv",
        BRACKET_DEPTH,
    ],
};

impl TargetConv {
    /// The record string, byte-equal to Python's `MAC_RECORD` (`dmsh_build.py`): it is part of the reflection
    /// JSON (`chain_metal`) and of the Metal cache identity. It does not name `BRACKET_DEPTH`.
    pub fn record(&self) -> String {
        let sdk = self.target.sdk();
        let flags: Vec<&str> = self
            .compile_flags
            .iter()
            .copied()
            .filter(|f| *f != BRACKET_DEPTH)
            .collect();
        format!(
            "xcrun -sdk {sdk} metal {}; xcrun -sdk {sdk} metallib",
            flags.join(" ")
        )
    }

    /// The flags for building the precompiled header: the compile flags without `-c` (as `dmsh_pch.make`).
    pub fn pch_flags(&self) -> Vec<&'static str> {
        self.compile_flags
            .iter()
            .copied()
            .filter(|f| *f != "-c")
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_record_matches_python() {
        assert_eq!(
            MAC.record(),
            "xcrun -sdk macosx metal -c -O1 -fmetal-math-mode=relaxed -fpreserve-invariance -fwrapv; xcrun -sdk macosx metallib"
        );
        assert_eq!(
            MAC.pch_flags(),
            [
                "-O1",
                "-fmetal-math-mode=relaxed",
                "-fpreserve-invariance",
                "-fwrapv",
                "-fbracket-depth=1024"
            ]
        );
        assert_eq!(
            IOS.record(),
            "xcrun -sdk iphoneos metal -c -O1 -mios-version-min=18.0 -fmetal-math-mode=relaxed -fpreserve-invariance -fwrapv; xcrun -sdk iphoneos metallib"
        );
    }

    #[test]
    fn standalone_ios_target_is_separate_from_mac_and_legacy_ios() {
        assert_eq!(IOS26.target, Target::Ios);
        assert!(IOS26.record().contains("-sdk iphoneos"));
        assert!(IOS26.record().contains("-mios-version-min=26.0"));
        assert_ne!(IOS26.record(), MAC.record());
        assert_ne!(IOS26.record(), IOS.record());
        assert!(IOS26.pch_flags().contains(&"-mios-version-min=26.0"));
    }

    #[test]
    fn both_targets_keep_nan_and_inf() {
        for conv in [&MAC, &IOS, &IOS26] {
            assert!(conv.compile_flags.contains(&"-fmetal-math-mode=relaxed"));
            assert!(conv.pch_flags().contains(&"-fmetal-math-mode=relaxed"));
        }
    }

    #[test]
    fn both_targets_wrap_signed_overflow() {
        for conv in [&MAC, &IOS, &IOS26] {
            assert!(conv.compile_flags.contains(&"-fwrapv"));
            assert!(conv.pch_flags().contains(&"-fwrapv"));
            assert!(conv.record().split([' ', ';']).any(|f| f == "-fwrapv"));
        }
    }
}
