//! `shader_scope groups`: which files the split reads.
#![allow(clippy::unwrap_used)]

use nxb_split::shaders::Profile;

const ZST: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

#[test]
fn a_compressed_file_outside_the_groups_is_left_alone() {
    let p = Profile::from_game_tsv("shader_groups\t/Shader/=shader /Effect/=effect\nshader_scope\tgroups\n");
    // in a group: read, compressed or not
    assert!(p.reads("/Shader/a.bfsha.zs", &ZST));
    assert!(p.reads("/Effect/x/b.byml.zs", &ZST));
    // outside: only the compressed one is left alone
    assert!(!p.reads("/Model/a.bfres.zs", &ZST));
    assert!(!p.reads("/Model/a.bin", &[0, 0, 0, 0, 0x28, 0xb5, 0x2f, 0xfd]));
    assert!(p.reads("/Model/a.webm", b"\x1aE\xdf\xa3 plain"));
}

#[test]
fn without_the_scope_every_file_is_read() {
    let p = Profile::from_game_tsv("shader_groups\t/Shader/=shader\n");
    assert!(p.reads("/Model/a.bfres.zs", &ZST));
}
