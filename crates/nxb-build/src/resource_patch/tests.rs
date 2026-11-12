use super::*;

/// A little-endian SARC with one member per (name, bytes), each at a 4-aligned data offset.
fn sarc(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut names = Vec::new();
    let mut data = Vec::new();
    let mut sfat = Vec::new();
    for (name, bytes) in members {
        let attr = 0x0100_0000u32 | (names.len() / 4) as u32;
        names.extend_from_slice(name.as_bytes());
        names.push(0);
        names.resize(names.len().div_ceil(4) * 4, 0);
        data.resize(data.len().div_ceil(4) * 4, 0);
        sfat.extend_from_slice(&[0; 4]);
        sfat.extend_from_slice(&attr.to_le_bytes());
        sfat.extend_from_slice(&(data.len() as u32).to_le_bytes());
        data.extend_from_slice(bytes);
        sfat.extend_from_slice(&(data.len() as u32).to_le_bytes());
    }
    let off = 0x14 + 0xc + sfat.len() + 8 + names.len();
    let mut d = Vec::new();
    d.extend_from_slice(b"SARC");
    d.extend_from_slice(&0x14u16.to_le_bytes());
    d.extend_from_slice(&[0xff, 0xfe]);
    d.extend_from_slice(&((off + data.len()) as u32).to_le_bytes());
    d.extend_from_slice(&(off as u32).to_le_bytes());
    d.extend_from_slice(&[0; 4]);
    d.extend_from_slice(b"SFAT");
    d.extend_from_slice(&0xcu16.to_le_bytes());
    d.extend_from_slice(&(members.len() as u16).to_le_bytes());
    d.extend_from_slice(&0x65u32.to_le_bytes());
    d.extend_from_slice(&sfat);
    d.extend_from_slice(b"SFNT");
    d.extend_from_slice(&8u16.to_le_bytes());
    d.extend_from_slice(&[0; 2]);
    d.extend_from_slice(&names);
    d.extend_from_slice(&data);
    d
}

const OFFSETS: &[usize] = &[4, 12];

fn patch(archive: Digest, member: Digest) -> ResourcePatch {
    ResourcePatch {
        option: "resolution-scale",
        about: "test",
        archive: "Pack/Boot.pack.zs",
        archive_sha256: archive,
        dictionary: None,
        member: "Dir/param.bgyml",
        member_sha256: member,
        edit: Edit::F32Words {
            offsets: OFFSETS,
            above: 0.0,
            up_to: 1.0,
        },
        default: None,
    }
}

#[test]
fn options_pick_values_and_check_them() {
    let p = patch(Digest([0; 32]), Digest([0; 32]));
    let o = |v: &[(&str, &str)]| -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    };
    // no option, no default: nothing
    assert!(choose(&[p], &[]).unwrap().0.is_empty());
    let (c, w) = choose(&[p], &o(&[("resolution-scale", "0.5"), ("other", "1")])).unwrap();
    assert_eq!(c.iter().map(|c| c.1).collect::<Vec<_>>(), [0.5]);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(choose(&[p], &o(&[("resolution-scale", "1.5")])).is_err());
    assert!(choose(&[p], &o(&[("resolution-scale", "x")])).is_err());
    let with_default = ResourcePatch {
        default: Some(0.75),
        ..p
    };
    assert_eq!(choose(&[with_default], &[]).unwrap().0[0].1, 0.75);
    let stock = o(&[("resolution-scale", "stock")]);
    assert!(choose(&[with_default], &stock).unwrap().0.is_empty());
}

#[test]
fn the_member_words_change_and_nothing_else() {
    let member: Vec<u8> = (0..16u8).collect();
    let s = sarc(&[("a.bin", b"other"), ("Dir/param.bgyml", &member)]);
    let raw = nxb_split::zstd::raw_frame(&s);
    let p = patch(sha(&raw), sha(&member));
    let read = |path: &str| Ok((path == "Pack/Boot.pack.zs").then(|| raw.clone()));
    let (files, warnings) = edited(&read, &[(p, 0.5)]);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(files.len(), 1);
    let out = nxb_split::zstd::decompress(&files[0].1).unwrap();
    assert_eq!(out.len(), s.len());
    let r = nxb_romfs::sarc::member(&out, "Dir/param.bgyml").unwrap();
    let mut want = member.clone();
    for &o in OFFSETS {
        want[o..o + 4].copy_from_slice(&0.5f32.to_le_bytes());
    }
    assert_eq!(&out[r.clone()], want.as_slice());
    // the bytes outside the member are as they were
    assert_eq!(&out[..r.start], &s[..r.start]);
    assert_eq!(&out[r.end..], &s[r.end..]);
}

#[test]
fn other_digests_are_skipped_with_a_warning() {
    let member = [0u8; 16];
    let s = sarc(&[("Dir/param.bgyml", &member)]);
    let raw = nxb_split::zstd::raw_frame(&s);
    let read = |_: &str| Ok(Some(raw.clone()));
    let (files, warnings) = edited(&read, &[(patch(Digest([1; 32]), sha(&member)), 0.5)]);
    assert!(files.is_empty() && warnings.len() == 1, "{warnings:?}");
    let (files, warnings) = edited(&read, &[(patch(sha(&raw), Digest([1; 32])), 0.5)]);
    assert!(files.is_empty() && warnings.len() == 1, "{warnings:?}");
    let none = |_: &str| Ok(None);
    let (files, warnings) = edited(&none, &[(patch(sha(&raw), sha(&member)), 0.5)]);
    assert!(files.is_empty() && warnings.len() == 1, "{warnings:?}");
}

#[test]
fn explicit_options_never_apply_defaults_and_reject_unknown_names() {
    let p = ResourcePatch {
        default: Some(0.75),
        ..patch(Digest([0; 32]), Digest([0; 32]))
    };
    let other = ResourcePatch {
        option: "other",
        ..p
    };
    assert!(choose_explicit(&[p, other], &[]).unwrap().is_empty());
    let options = vec![("resolution-scale".into(), "0.5".into())];
    let chosen = choose_explicit(&[p, other], &options).unwrap();
    assert_eq!(chosen.len(), 1);
    assert_eq!(chosen[0].0.option, "resolution-scale");
    assert_eq!(chosen[0].1, 0.5);
    for value in ["stock", "1"] {
        let unknown = vec![("unknown".into(), value.into())];
        assert!(choose_explicit(&[p], &unknown).is_err());
        assert!(choose_explicit(&[], &unknown).is_err());
    }
    let stock = vec![("resolution-scale".into(), "stock".into())];
    assert!(choose_explicit(&[p], &stock).unwrap().is_empty());
    let mut duplicate = options;
    duplicate.extend(stock);
    assert!(choose_explicit(&[p], &duplicate).unwrap().is_empty());
    for value in ["1.5", "x", "NaN"] {
        let invalid = vec![("resolution-scale".into(), value.into())];
        assert!(choose_explicit(&[p], &invalid).is_err());
    }
}
