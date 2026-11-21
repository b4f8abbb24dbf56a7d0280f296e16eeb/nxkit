//! Tests for the handles that shaders read from uniform buffers.

use super::*;

fn json(text: &str) -> Json {
    crate::json::parse(text).expect("json")
}

fn ubo(slot: u32, address: u64, size: u32, snapshot: Option<&[u8]>) -> UboDetail {
    UboDetail {
        stage: Stage::Fragment,
        slot,
        address,
        size,
        snapshot: snapshot.map(Arc::from),
    }
}

#[test]
fn a_combined_handle_reads_one_word() {
    let b = json(r#"{"kind": "tex_ubo", "reg": 4, "off": 128}"#);
    let src = source(&b, false).expect("source");
    assert_eq!((src.tex, src.samp), ((4, 128), (4, 128)));
    let s = json(r#"{"kind": "samp_ubo", "reg": 4, "off": 128, "border_bit": 11}"#);
    assert_eq!(source(&s, true), Some(src));
}

#[test]
fn a_split_handle_names_both_words_from_either_member() {
    // tex_ubo: its own word is the texture, the other half the sampler.
    let t = json(r#"{"kind": "tex_ubo", "reg": 1, "off": 28, "sreg": 3, "soff": 8}"#);
    // samp_ubo: its own word is the sampler, the other half the texture.
    let s = json(r#"{"kind": "samp_ubo", "reg": 3, "off": 8, "sreg": 1, "soff": 28}"#);
    let want = Source {
        tex: (1, 28),
        samp: (3, 8),
    };
    assert_eq!(source(&t, false), Some(want));
    assert_eq!(source(&s, true), Some(want));
}

#[test]
fn a_sampler_word_in_c2_or_a_missing_offset_is_refused() {
    let c2 = json(r#"{"kind": "samp_ubo", "reg": 3, "off": 28, "sreg": 255, "soff": 368}"#);
    assert_eq!(source(&c2, true), None);
    assert_eq!(
        source(&json(r#"{"kind": "tex_ubo", "reg": 3}"#), false),
        None
    );
    let null = json(r#"{"kind": "tex_ubo", "reg": 2, "off": 4, "sreg": null, "soff": null}"#);
    assert_eq!(source(&null, false).map(|s| s.samp), Some((2, 4)));
}

#[test]
fn the_texture_index_and_the_sampler_index_come_from_their_words() {
    assert_eq!(combine(0x0030_0123, 0x0030_0123), 0x0030_0123);
    assert_eq!(combine(0xABC0_0042, 0x0050_0999), 0x0050_0042);
}

#[test]
fn each_source_gets_one_bind_with_the_handle_of_the_draw() {
    let src = Source {
        tex: (4, 8),
        samp: (4, 8),
    };
    // A tex_ubo and a samp_ubo member of the same source.
    let binds = [(SLOT_BASE, src), (SLOT_BASE, src)];
    let mut bytes = [0u8; 16];
    bytes[8..12].copy_from_slice(&0x0010_0007u32.to_le_bytes());
    // The snapshot wins over guest memory.
    let ubos = [ubo(4, 0x1000, 16, Some(&bytes))];
    let mut never = |_: u64, _: &mut [u8]| -> bool { panic!("read") };
    let mut out = vec![TextureBind {
        stage: Stage::Fragment,
        slot: 2,
        handle: 9,
    }];
    binds_of(&binds, Stage::Fragment, &ubos, &mut never, &mut out);
    assert_eq!(
        out[1],
        TextureBind {
            stage: Stage::Fragment,
            slot: SLOT_BASE,
            handle: 0x0010_0007,
        }
    );
    assert_eq!(out.len(), 2);
}

#[test]
fn without_a_snapshot_the_word_comes_from_guest_memory() {
    let src = Source {
        tex: (0, 4),
        samp: (1, 0),
    };
    let binds = [(SLOT_BASE, src)];
    let ubos = [ubo(0, 0x2000, 8, None), ubo(1, 0x3000, 4, None)];
    let mut read = |a: u64, out: &mut [u8]| {
        let w: u32 = match a {
            0x2004 => 0xFFF0_0055,
            0x3000 => 0x0020_0FFF,
            _ => return false,
        };
        out.copy_from_slice(&w.to_le_bytes());
        true
    };
    let mut out = Vec::new();
    binds_of(&binds, Stage::Fragment, &ubos, &mut read, &mut out);
    assert_eq!(out[0].handle, 0x0020_0055);
}

#[test]
fn an_unbound_slot_or_a_word_past_the_end_gives_the_null_handle() {
    let past = Source {
        tex: (0, 8),
        samp: (0, 8),
    };
    let unbound = Source {
        tex: (5, 0),
        samp: (5, 0),
    };
    let binds = [(SLOT_BASE, past), (SLOT_BASE + 1, unbound)];
    let ubos = [ubo(0, 0x2000, 8, Some(&[1u8; 8]))];
    let mut read = |_: u64, _: &mut [u8]| true;
    let mut out = Vec::new();
    binds_of(&binds, Stage::Fragment, &ubos, &mut read, &mut out);
    assert_eq!(
        out.iter().map(|t| (t.slot, t.handle)).collect::<Vec<_>>(),
        vec![(SLOT_BASE, 0), (SLOT_BASE + 1, 0)]
    );
}
