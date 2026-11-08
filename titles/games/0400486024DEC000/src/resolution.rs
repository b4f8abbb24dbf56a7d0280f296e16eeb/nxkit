//! A fixed resolution scale for v131072 (`--resource-patch resolution-scale=<scale>`).
//!
//! Found by ComicBit (v0lumehi/nxkit#22): the two dynamic-resolution palettes of the game share ten scale words in
//! the bootup pack's ModelSceneExtensionParam. Setting all ten to one scale pins the resolution. The thresholds,
//! the initialization and the timing stay as they are. The digests are those of v131072. No game data is stored here.

use nxtitle::{Digest, Edit, Evidence, Fact, ResourcePatch, ZstdDictionary};

/// The patch. Without the option the game keeps its dynamic resolution.
pub(super) const PATCHES: &[Fact<ResourcePatch>] = &[Fact::new(
    ResourcePatch {
        option: "resolution-scale",
        about: "pins the dynamic-resolution scale (0 < scale <= 1)",
        archive: "Pack/Bootup.Nin_Ounce_NVN2.pack.zs",
        archive_sha256: ARCHIVE,
        dictionary: Some(ZstdDictionary {
            archive: "Pack/ZsDic.pack.zs",
            member: "pack.zsdic",
            sha256: DICTIONARY,
        }),
        member: "System/GameGraphics/ModelSceneExtensionParam/Default.game__gfx__ModelSceneExtensionParam.bgyml",
        member_sha256: MEMBER,
        edit: Edit::F32Words {
            offsets: &[3136, 3156, 3176, 3196, 3216, 3236, 3256, 3276, 3296, 3316],
            above: 0.0,
            up_to: 1.0,
        },
        default: None,
    },
    Evidence::Recorded {
        source: "https://github.com/v0lumehi/nxkit/pull/22 (ComicBit)",
        read: "2026-11-08",
    },
)];

const ARCHIVE: Digest = Digest([
    0x10, 0x7a, 0xec, 0xcf, 0xd8, 0x60, 0x93, 0x93, 0x71, 0x84, 0x1d, 0xb6, 0x96, 0xed, 0x27, 0x8f,
    0x2b, 0xf3, 0x0d, 0x94, 0x17, 0x89, 0xbd, 0x5c, 0xfb, 0x91, 0xa4, 0x1f, 0x48, 0xb8, 0x33, 0x62,
]);
const MEMBER: Digest = Digest([
    0xcf, 0xaf, 0xd2, 0xc0, 0x41, 0xde, 0xf3, 0x70, 0x49, 0x03, 0xc2, 0x3f, 0x65, 0xc6, 0x6b, 0x89,
    0xf2, 0x52, 0x9b, 0xbc, 0xac, 0x63, 0x84, 0xd4, 0x17, 0x73, 0x5e, 0x1e, 0xa2, 0xd5, 0xa6, 0x78,
]);
const DICTIONARY: Digest = Digest([
    0xf6, 0x18, 0xf9, 0xa8, 0xa2, 0xfe, 0x32, 0x52, 0xda, 0xac, 0xf3, 0x2e, 0xdc, 0x89, 0xfd, 0x96,
    0x2a, 0x92, 0x8b, 0xfd, 0x09, 0x04, 0x02, 0x5e, 0x27, 0xfe, 0x86, 0x9c, 0x98, 0x53, 0x56, 0xd8,
]);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_patch_is_off_by_default_and_takes_scales_up_to_one() {
        let p = PATCHES[0].value;
        assert_eq!(p.default, None);
        assert!(PATCHES[0].evidence.is_checked());
        assert!(p.edit.accepts(0.5) && p.edit.accepts(1.0));
        assert!(!p.edit.accepts(0.0) && !p.edit.accepts(1.5) && !p.edit.accepts(f32::NAN));
    }
}
