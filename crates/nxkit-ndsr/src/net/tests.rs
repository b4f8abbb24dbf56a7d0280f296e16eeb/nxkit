use super::*;

#[test]
fn counts_per_pack() {
    let n = |p| NETS.iter().filter(|n| n.pack == p).count();
    assert_eq!((n(Pack::Agl), n(Pack::Vr), n(Pack::VrLight)), (22, 15, 15));
}

#[test]
fn keys_are_unique_lower_case_hex() {
    for (i, a) in NETS.iter().enumerate() {
        assert_eq!(a.key.len(), 16);
        assert!(
            a.key
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        );
        assert!(NETS[i + 1..].iter().all(|b| b.key != a.key), "{}", a.key);
    }
}

#[test]
fn blobs_belong_to_their_pack() {
    for n in &NETS {
        let b = n.blob();
        assert_eq!(b.pack, n.pack, "{}", n.key);
        assert!(b.len == 0x3000 || b.len == 0x1800);
        assert_eq!(b.sha256.len(), 64);
    }
    // The five vr blobs of the notes, by ratio.
    let (_, n) = Net::by_key("14C10AA70B81F04E").unwrap();
    assert_eq!(
        n.blob().sha256,
        "b82b7ec1326f05d6d0ce143b815de0779849ebbb4d40f77c0fba7821979c39b0"
    );
    let (_, n) = Net::by_key("aec4dfce7ab3cefa").unwrap();
    assert_eq!(
        n.blob().sha256,
        "5de06ec356888082e6f04e266767894b30dce661872146c18c28efde6fbda30a"
    );
    assert!(Net::by_key("0000000000000000").is_none());
}

#[test]
fn the_4_3_blobs_have_three_layers() {
    for key in ["aa1243f635baa8e8", "927ceaf3cf1be240"] {
        let (_, n) = Net::by_key(key).unwrap();
        assert_eq!(n.blob().layers(), 3);
        assert_eq!((n.in_px, n.out_px), (6, 8));
    }
    let (_, n) = Net::by_key("14c10aa70b81f04e").unwrap();
    assert_eq!(n.blob().layers(), 6);
}

#[test]
fn vr_geometry_matches_the_notes() {
    // Section 7.1: 1.2 A is a 46 x 36 window from (39 wx - 3, 29 wy - 3), 4 x 3 patches, a 48 x 36 grid.
    let (_, n) = Net::by_key("14c10aa70b81f04e").unwrap();
    assert_eq!((n.step_x, n.step_y, n.margin()), (39, 29, 3));
    assert_eq!((n.patches_x(), n.patches_y()), (4, 3));
    assert_eq!(n.window_size(), (46, 36));
    assert_eq!(n.grid_size(), (48, 36));
    assert_eq!(n.config(), [0x27, 0x1d, 0xb9, 0x22]);
    // Section 4: margins 3, 4, 4, 5, 6 for in = 10, 8, 7, 6, 4.
    let m: Vec<usize> = [10u8, 8, 7, 6, 4]
        .iter()
        .map(|i| {
            NETS.iter()
                .find(|n| n.pack == Pack::Vr && n.in_px == *i)
                .unwrap()
                .margin()
        })
        .collect();
    assert_eq!(m, [3, 4, 4, 5, 6]);
}

#[test]
fn vr_immediates() {
    for n in NETS.iter().filter(|n| n.pack == Pack::Vr) {
        let c = n.vr.unwrap();
        assert_eq!(c.y_every_row, n.in_px == 10, "{}", n.key);
        // The ratio bits are f32(out / in), except 12/7 which is one ulp above.
        let want = (f64::from(n.out_px) / f64::from(n.in_px)) as f32;
        let d = i64::from(c.ratio_bits) - i64::from(want.to_bits());
        assert_eq!(d, i64::from(n.in_px == 7), "{}", n.key);
        // s is about 1 / ratio and h about s / 2.
        let s = f32::from_bits(c.step_bits);
        assert!((f64::from(s) * n.ratio() - 1.0).abs() < 1e-6);
        assert!((f64::from(f32::from_bits(c.half_bits)) * 2.0 - f64::from(s)).abs() < 1e-6);
    }
    assert!(
        NETS.iter()
            .filter(|n| n.pack != Pack::Vr)
            .all(|n| n.vr.is_none())
    );
}

#[test]
fn variants_follow_the_entries() {
    // Within one blob the programs are A, B (, C).
    for w in NETS.windows(2) {
        if w[0].blob == w[1].blob {
            assert_ne!(w[0].variant, w[1].variant);
        }
    }
    let vr: Vec<_> = NETS
        .iter()
        .filter(|n| n.pack == Pack::Vr)
        .map(|n| n.variant)
        .collect();
    assert_eq!(vr.chunks(3).filter(|c| *c == [A, B, C]).count(), 5);
}

#[test]
fn pack_facts() {
    for p in Pack::ALL {
        assert_eq!(p.sha256().len(), 64);
        assert_eq!(p.header_words()[2], p.entry_count());
    }
    assert_eq!(Pack::Vr.byte_len(), 0x99000);
}
