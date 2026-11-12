//! Shared authored sparse NCA and BKTR update fixture. No game data or real keys.
#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::common::*;
use nxb_extract::aes::Aes128;

pub const APP: u64 = 0x0100_0000_0000_2000;
const NODE: usize = 0x4000;

fn rom() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("a.bin", vec![0x11; 0x8000]),
        ("b.bin", (0..0x10000u32).map(|i| (i * 7) as u8).collect()),
        ("c.bin", vec![0x33; 0x8000]),
    ]
}

fn exefs(tag: u8) -> Section {
    pfs0_section(&[
        ("main".to_string(), vec![tag; 0x500]),
        ("main.npdm".into(), npdm(APP)),
    ])
}

/// A bucket tree of one entry set: the offset node, then the entry node.
fn bucket_tree(entry_size: usize, entries: &[Vec<u8>], end: u64) -> Vec<u8> {
    let mut t = vec![0u8; 2 * NODE];
    t[4..8].copy_from_slice(&1u32.to_le_bytes());
    t[8..16].copy_from_slice(&end.to_le_bytes());
    t[NODE + 4..NODE + 8].copy_from_slice(&(entries.len() as u32).to_le_bytes());
    for (i, e) in entries.iter().enumerate() {
        assert_eq!(e.len(), entry_size);
        t[NODE + 16 + i * entry_size..NODE + 16 + (i + 1) * entry_size].copy_from_slice(e);
    }
    t
}

fn indirect(virt: u64, phys: u64, source: u32) -> Vec<u8> {
    let mut e = virt.to_le_bytes().to_vec();
    e.extend_from_slice(&phys.to_le_bytes());
    e.extend_from_slice(&source.to_le_bytes());
    e
}

/// The counter's upper half as the NCA code derives it from the FS header's bytes at 0x140.
fn upper_of(fs: &[u8]) -> [u8; 8] {
    let mut u: [u8; 8] = fs[0x140..0x148].try_into().unwrap();
    u.reverse();
    u
}

pub struct Fixture {
    pub base: Vec<u8>,
    pub update: Vec<u8>,
    pub image: Vec<(String, Vec<u8>)>,
}

/// A base NSP whose RomFS section lacks `[hole.0, hole.1)`, and an update NSP that supplies that range.
pub fn fixture() -> Fixture {
    let hole = (0x10000u64, 0x18000u64);
    let data = romfs_section(&rom());
    let full = data.bytes.clone();
    let virt = full.len() as u64;
    assert!(virt > hole.1 && virt.is_multiple_of(NODE as u64));
    let ex = exefs(3);
    let sec0_len = ex.bytes.len() as u64;
    let start = 0x4000 + sec0_len;

    // the base: data ranges, then the table
    let kept = hole.0 + (virt - hole.1);
    let table = bucket_tree(
        0x14,
        &[
            indirect(0, 0, 0),
            indirect(hole.0, 0, 1),
            indirect(hole.1, hole.0, 0),
        ],
        virt,
    );
    let phys_off = start + virt;
    let mut fs = data.fs.clone();
    fs[0x148..0x150].copy_from_slice(&kept.to_le_bytes());
    fs[0x150..0x158].copy_from_slice(&(table.len() as u64).to_le_bytes());
    fs[0x158..0x15C].copy_from_slice(b"BKTR");
    fs[0x15C..0x160].copy_from_slice(&1u32.to_le_bytes());
    fs[0x160..0x164].copy_from_slice(&3u32.to_le_bytes());
    fs[0x168..0x170].copy_from_slice(&phys_off.to_le_bytes());
    fs[0x170..0x172].copy_from_slice(&1u16.to_le_bytes());
    let sparse_fs = fs.clone();
    let (mut prog, _) = nca(
        0,
        APP,
        vec![
            (0, ex),
            (
                1,
                Section {
                    bytes: data.bytes,
                    fs,
                },
            ),
        ],
    );
    let (s, e) = (start as usize, (start + virt) as usize);
    let ct = prog[s..e].to_vec();
    prog[s..e].fill(0);
    let mut phys = ct[..hole.0 as usize].to_vec();
    phys.extend_from_slice(&ct[hole.1 as usize..]);
    assert_eq!(phys.len() as u64, kept);
    let mut meta = table;
    let v = (u64::from_be_bytes(upper_of(&sparse_fs)) & !0xffff_ffff) | (1 << 16);
    Aes128::new(&CTR_KEY).ctr(v.to_be_bytes(), phys_off + kept, &mut meta);
    phys.extend_from_slice(&meta);
    assert_eq!(prog.len() as u64, phys_off);
    prog.extend_from_slice(&phys);
    let base = pkg(&prog, 0x80, APP, 0);

    // the update: the hole's data, the indirect table and the counter table
    let patch_data = full[hole.0 as usize..hole.1 as usize].to_vec();
    let d = patch_data.len() as u64;
    let ind = bucket_tree(
        0x14,
        &[
            indirect(0, 0, 0),
            indirect(hole.0, 0, 1),
            indirect(hole.1, hole.1, 0),
        ],
        virt,
    );
    let mut ctr_entry = 0u64.to_le_bytes().to_vec();
    ctr_entry.extend_from_slice(&[0, 0, 0, 0]);
    ctr_entry.extend_from_slice(&5u32.to_le_bytes());
    let ctr = bucket_tree(0x10, &[ctr_entry], d);
    let pfs = fs_patch(&sparse_fs, d, ind.len() as u64, ctr.len() as u64);
    let mut bytes = patch_data;
    bytes.extend_from_slice(&ind);
    bytes.extend_from_slice(&ctr);
    let len = bytes.len();
    let (mut up, _) = nca(
        0,
        APP,
        vec![
            (0, exefs(4)),
            (
                1,
                Section {
                    bytes: vec![0; len],
                    fs: pfs,
                },
            ),
        ],
    );
    let upd_start = 0x4000 + exefs(4).bytes.len();
    let upd_end = upd_start + len;
    let plain = upper_of(&sparse_fs);
    let mut gen5 = plain;
    gen5[4..8].copy_from_slice(&5u32.to_be_bytes());
    let aes = Aes128::new(&CTR_KEY);
    aes.ctr(gen5, upd_start as u64, &mut bytes[..d as usize]);
    aes.ctr(plain, upd_start as u64 + d, &mut bytes[d as usize..]);
    up[upd_start..upd_end].copy_from_slice(&bytes);
    let update = pkg(&up, 0x81, APP | 0x800, 1 << 16);

    let image = {
        let mut v: Vec<_> = rom()
            .into_iter()
            .map(|(n, b)| (format!("/{n}"), b))
            .collect();
        v.sort();
        v
    };
    Fixture {
        base,
        update,
        image,
    }
}

/// The update's FS header: the data section's IVFC part with a BKTR patch part.
fn fs_patch(base_fs: &[u8], d: u64, ind_size: u64, ctr_size: u64) -> Vec<u8> {
    let mut fs = base_fs.to_vec();
    fs[0x148..0x172].fill(0);
    fs[4] = 4;
    fs[0x100..0x108].copy_from_slice(&d.to_le_bytes());
    fs[0x108..0x110].copy_from_slice(&ind_size.to_le_bytes());
    fs[0x110..0x114].copy_from_slice(b"BKTR");
    fs[0x114..0x118].copy_from_slice(&1u32.to_le_bytes());
    fs[0x118..0x11C].copy_from_slice(&3u32.to_le_bytes());
    fs[0x120..0x128].copy_from_slice(&(d + ind_size).to_le_bytes());
    fs[0x128..0x130].copy_from_slice(&ctr_size.to_le_bytes());
    fs[0x130..0x134].copy_from_slice(b"BKTR");
    fs[0x134..0x138].copy_from_slice(&1u32.to_le_bytes());
    fs[0x138..0x13C].copy_from_slice(&1u32.to_le_bytes());
    fs
}

/// An NSP of a program NCA and the meta NCA that lists it.
fn pkg(program: &[u8], kind: u8, title: u64, version: u32) -> Vec<u8> {
    let pid = [if kind == 0x80 { 7u8 } else { 8 }; 16];
    let meta = pfs0_section(&[("m.cnmt".into(), cnmt(kind, title, version, &[pid]))]);
    let (m, _) = nca(1, APP, vec![(0, meta)]);
    pfs0(&[
        (format!("{}.nca", hex(&pid)), program.to_vec()),
        (format!("{}.cnmt.nca", hex(&[9u8; 16])), m),
    ])
}
