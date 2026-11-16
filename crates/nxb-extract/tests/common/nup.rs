//! Synthetic NUP containers shared by extraction tests. No game data or keys.
#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

/// One tree node: a folder with children or a file with content (stored raw, or as a zstd frame when `z`).
pub enum Node {
    Dir(&'static str, Vec<Node>),
    File(&'static str, Vec<u8>, bool),
}

pub struct Pkg {
    pub title: u64,
    pub kind: u8,
    pub version: u32,
    pub flags: u8,
    pub exefs: Option<Vec<Node>>,
    pub romfs: Option<Vec<Node>>,
    pub meta: Vec<(u16, Vec<u8>)>,
}

/// A zstd frame of one raw block (what a packer writes for incompressible data).
fn zstd_raw(data: &[u8]) -> Vec<u8> {
    let mut f = vec![0x28, 0xb5, 0x2f, 0xfd, 0xe0];
    f.extend_from_slice(&(data.len() as u64).to_le_bytes());
    let h = ((data.len() as u32) << 3) | 1;
    f.extend_from_slice(&h.to_le_bytes()[..3]);
    f.extend_from_slice(data);
    f
}

#[derive(Default)]
struct Builder {
    dirs: Vec<(Vec<u8>, Vec<u32>)>,
    files: Vec<(Vec<u8>, Vec<u8>, bool)>,
}

impl Builder {
    /// Adds a folder and returns its index, children are slot values (folders: index, files: dirs + index, fixed later).
    fn add(&mut self, name: &str, kids: &[Node]) -> u32 {
        let me = self.dirs.len() as u32;
        self.dirs.push((name.as_bytes().to_vec(), Vec::new()));
        let mut slots = Vec::new();
        for k in kids {
            match k {
                Node::Dir(n, c) => slots.push(self.add(n, c)),
                Node::File(n, d, z) => {
                    self.files.push((n.as_bytes().to_vec(), d.clone(), *z));
                    slots.push(0x8000_0000 | (self.files.len() - 1) as u32);
                }
            }
        }
        self.dirs[me as usize].1 = slots;
        me
    }
}

pub fn build(pkgs: &[Pkg], corrupt_toc: bool) -> Vec<u8> {
    let mut b = Builder::default();
    let mut recs = Vec::new();
    for p in pkgs {
        let ex = p.exefs.as_ref().map(|k| b.add("exefs", k));
        let ro = p.romfs.as_ref().map(|k| b.add("romfs", k));
        let root = b.add("root", &[]);
        let mut slots = Vec::new();
        slots.extend(ex);
        slots.extend(ro);
        b.dirs[root as usize].1 = slots;
        recs.push((root, ex.unwrap_or(u32::MAX), ro.unwrap_or(u32::MAX)));
    }
    let nd = b.dirs.len() as u32;
    let mut pool = Vec::new();
    let mut name = |n: &[u8]| {
        let o = pool.len() as u32;
        pool.extend_from_slice(n);
        (o, n.len() as u32)
    };
    let names_d: Vec<(u32, u32)> = b.dirs.iter().map(|d| name(&d.0)).collect();
    let names_f: Vec<(u32, u32)> = b.files.iter().map(|f| name(&f.0)).collect();
    let mut slots = Vec::new();
    let mut firsts = Vec::new();
    for d in &b.dirs {
        firsts.push((slots.len() as u32, d.1.len() as u32));
        for &s in &d.1 {
            slots.push(if s & 0x8000_0000 != 0 {
                nd + (s & 0x7fff_ffff)
            } else {
                s
            });
        }
    }
    let toc_len_fixed =
        0x30 + pkgs.len() * 0x40 + b.dirs.len() * 0x18 + b.files.len() * 0x50 + slots.len() * 4;
    let pool_off = toc_len_fixed;
    let toc_size = pool_off + pool.len();
    let blob = (0x80 + toc_size).div_ceil(16) * 16;
    // data region: files, then meta blocks
    let mut data = Vec::new();
    let mut extents = Vec::new();
    for (_, d, z) in &b.files {
        if d.is_empty() {
            extents.push((0u64, 0u64, false));
            continue;
        }
        while !(blob + data.len()).is_multiple_of(16) {
            data.push(0);
        }
        let stored = if *z { zstd_raw(d) } else { d.clone() };
        extents.push(((blob + data.len()) as u64, stored.len() as u64, *z));
        data.extend_from_slice(&stored);
    }
    let mut metas = Vec::new();
    for p in pkgs {
        let mut m = Vec::new();
        for (t, v) in &p.meta {
            m.extend_from_slice(&t.to_le_bytes());
            m.extend_from_slice(&0u16.to_le_bytes());
            m.extend_from_slice(&(v.len() as u32).to_le_bytes());
            m.extend_from_slice(v);
        }
        metas.push(if m.is_empty() {
            (0u64, 0u32)
        } else {
            ((blob + data.len()) as u64, m.len() as u32)
        });
        data.extend_from_slice(&m);
    }
    let mut toc = Vec::new();
    toc.extend_from_slice(b"TOC\x01");
    toc.extend_from_slice(&(pkgs.len() as u32).to_le_bytes());
    for v in [
        b.dirs.len() as u64,
        b.files.len() as u64,
        pool_off as u64,
        pool.len() as u64,
        0,
    ] {
        toc.extend_from_slice(&v.to_le_bytes());
    }
    for (i, p) in pkgs.iter().enumerate() {
        let (root, ex, ro) = recs[i];
        toc.extend_from_slice(&p.title.to_le_bytes());
        for v in [root, ex, ro, p.version] {
            toc.extend_from_slice(&v.to_le_bytes());
        }
        toc.extend_from_slice(&[p.kind, p.flags, 0, 0, 0, 0, 0, 0]);
        toc.extend_from_slice(&metas[i].0.to_le_bytes());
        toc.extend_from_slice(&metas[i].1.to_le_bytes());
        toc.extend_from_slice(&[0u8; 20]);
    }
    for (i, _) in b.dirs.iter().enumerate() {
        let (o, n) = names_d[i];
        for v in [o, n, firsts[i].0, firsts[i].1] {
            toc.extend_from_slice(&v.to_le_bytes());
        }
        toc.extend_from_slice(&0u64.to_le_bytes());
    }
    for (i, (_, d, _)) in b.files.iter().enumerate() {
        let (o, n) = names_f[i];
        toc.extend_from_slice(&o.to_le_bytes());
        toc.extend_from_slice(&n.to_le_bytes());
        let (off, stored, z) = extents[i];
        for v in [d.len() as u64, off, stored] {
            toc.extend_from_slice(&v.to_le_bytes());
        }
        toc.extend_from_slice(&nxb_model::sha256(d).0);
        toc.extend_from_slice(&[u8::from(z), 0, 0, 0]);
        toc.extend_from_slice(&[0u8; 12]);
    }
    for s in &slots {
        toc.extend_from_slice(&s.to_le_bytes());
    }
    toc.extend_from_slice(&pool);
    assert_eq!(toc.len(), toc_size);
    let mut sha = nxb_model::sha256(&toc).0;
    if corrupt_toc {
        sha[0] ^= 1;
    }
    let total = blob + data.len();
    let mut out = Vec::new();
    out.extend_from_slice(b"NUP\x01");
    out.extend_from_slice(&0x80u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0x80u64.to_le_bytes());
    out.extend_from_slice(&(toc_size as u64).to_le_bytes());
    out.extend_from_slice(&sha);
    out.extend_from_slice(&(blob as u64).to_le_bytes());
    out.extend_from_slice(&(total as u64).to_le_bytes());
    out.extend_from_slice(&(pkgs.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&[0u8; 48]);
    out.extend_from_slice(&toc);
    out.resize(blob, 0);
    out.extend_from_slice(&data);
    out
}
