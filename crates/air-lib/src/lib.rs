//! The metallib container (`MTLB`) as Apple's `metallib` (air-lld) writes it: header, function list,
//! header extension, public and private metadata, bitcode. Layout from the REA notes
//! (`rea-frontend/metallib-format.md`). [`write`] computes every derived field (sizes, offsets,
//! `HASH` = SHA-256 of each bitcode blob, `MDSZ`, `OFFT`); [`parse`] reads a library back into
//! parts, so `write(&parse(x)?) == x` for every `metallib` output.

#![deny(missing_docs)]

mod sha256;

pub use sha256::digest as sha256;

/// Header fields that do not depend on the functions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// 0x8001 macOS, 0x0001 iOS.
    pub platform: u16,
    /// File format version (2.9 for AIR 2.9, 2.8 for AIR 2.7).
    pub version: (u16, u16),
    /// 0 = executable.
    pub lib_type: u8,
    /// 0x81 macOS, 0x82 iOS.
    pub os: u8,
    /// Deployment target.
    pub os_version: (u16, u16),
}

impl Header {
    /// The header `metallib` writes for a module triple such as `air64_v29-apple-macosx27.2.0`.
    pub fn from_triple(triple: &str) -> Option<Header> {
        let (air, rest) = triple.strip_prefix("air64_v")?.split_once("-apple-")?;
        let air: u16 = air.parse().ok()?;
        let (platform, os, ver) = match rest.strip_prefix("macosx") {
            Some(v) => (0x8001, 0x81, v),
            None => (0x0001, 0x82, rest.strip_prefix("ios")?),
        };
        let mut it = ver.split('.').map(|x| x.parse::<u16>().ok());
        let maj = it.next()??;
        let min = it.next().flatten().unwrap_or(0);
        // file version: AIR 2.9 and 2.8 -> 2.9, AIR 2.7 -> 2.8, AIR 2.5 -> 2.7
        let minor = match air {
            29 | 28 => 9,
            27 => 8,
            25 => 7,
            _ => return None,
        };
        Some(Header {
            platform,
            version: (2, minor),
            lib_type: 0,
            os,
            os_version: (maj, min),
        })
    }
}

/// A tag: four-character name and data.
pub type Tag = ([u8; 4], Vec<u8>);

/// One function of the library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    /// Function list tags in order. `HASH`, `MDSZ` and `OFFT` are recomputed by [`write`]; their
    /// values here are ignored (but their positions are kept).
    pub tags: Vec<Tag>,
    /// Public metadata tags (`CNST`, `VATT`, `VATY` ...).
    pub public: Vec<Tag>,
    /// Private metadata tags.
    pub private: Vec<Tag>,
    /// The bitcode blob (wrapper header included).
    pub bitcode: Vec<u8>,
}

impl Function {
    /// A function with the standard tag order `NAME TYPE HASH MDSZ OFFT VERS`.
    pub fn new(name: &str, kind: u8, vers: [u16; 4], bitcode: Vec<u8>) -> Function {
        let mut n = name.as_bytes().to_vec();
        n.push(0);
        let v: Vec<u8> = vers.iter().flat_map(|x| x.to_le_bytes()).collect();
        Function {
            tags: vec![
                (*b"NAME", n),
                (*b"TYPE", vec![kind]),
                (*b"HASH", vec![0; 32]),
                (*b"MDSZ", vec![0; 8]),
                (*b"OFFT", vec![0; 24]),
                (*b"VERS", v),
            ],
            public: Vec::new(),
            private: Vec::new(),
            bitcode,
        }
    }

    /// The `NAME` tag without its NUL.
    pub fn name(&self) -> &[u8] {
        self.tags
            .iter()
            .find(|t| &t.0 == b"NAME")
            .map(|t| t.1.strip_suffix(&[0]).unwrap_or(&t.1))
            .unwrap_or(&[])
    }
}

/// A whole library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Library {
    /// Header.
    pub header: Header,
    /// Functions in order.
    pub functions: Vec<Function>,
    /// Header extension tags (empty for `metallib` output).
    pub extension: Vec<Tag>,
}

fn put_tags(out: &mut Vec<u8>, tags: &[Tag]) {
    for (n, v) in tags {
        out.extend_from_slice(n);
        out.extend_from_slice(&(v.len() as u16).to_le_bytes());
        out.extend_from_slice(v);
    }
    out.extend_from_slice(b"ENDT");
}

/// Writes the library as `metallib` does.
pub fn write(lib: &Library) -> Vec<u8> {
    let (mut fl, mut publ, mut priv_, mut bc) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for f in &lib.functions {
        let offs = (publ.len() as u64, priv_.len() as u64, bc.len() as u64);
        for (md, tags) in [(&mut publ, &f.public), (&mut priv_, &f.private)] {
            let mut body = Vec::new();
            put_tags(&mut body, tags);
            // metallib's record size does not count its own u32
            md.extend_from_slice(&(body.len() as u32).to_le_bytes());
            md.extend_from_slice(&body);
        }
        bc.extend_from_slice(&f.bitcode);
        let tags: Vec<Tag> = f
            .tags
            .iter()
            .map(|(n, v)| match n {
                b"HASH" => (*n, sha256::digest(&f.bitcode).to_vec()),
                b"MDSZ" => (*n, (f.bitcode.len() as u64).to_le_bytes().to_vec()),
                b"OFFT" => (
                    *n,
                    [offs.0, offs.1, offs.2]
                        .iter()
                        .flat_map(|x| x.to_le_bytes())
                        .collect(),
                ),
                _ => (*n, v.clone()),
            })
            .collect();
        let mut body = Vec::new();
        put_tags(&mut body, &tags);
        fl.extend_from_slice(&(body.len() as u32 + 4).to_le_bytes());
        fl.extend_from_slice(&body);
    }
    let mut ext = Vec::new();
    put_tags(&mut ext, &lib.extension);
    let fl_off = 0x58u64;
    let pub_off = fl_off + 4 + fl.len() as u64 + ext.len() as u64;
    let priv_off = pub_off + publ.len() as u64;
    let bc_off = priv_off + priv_.len() as u64;
    let total = bc_off + bc.len() as u64;
    let h = &lib.header;
    let mut out = Vec::with_capacity(total as usize);
    out.extend_from_slice(b"MTLB");
    out.extend_from_slice(&h.platform.to_le_bytes());
    out.extend_from_slice(&h.version.0.to_le_bytes());
    out.extend_from_slice(&h.version.1.to_le_bytes());
    out.push(h.lib_type);
    out.push(h.os);
    out.extend_from_slice(&h.os_version.0.to_le_bytes());
    out.extend_from_slice(&h.os_version.1.to_le_bytes());
    for x in [
        total,
        fl_off,
        fl.len() as u64,
        pub_off,
        publ.len() as u64,
        priv_off,
        priv_.len() as u64,
        bc_off,
        bc.len() as u64,
    ] {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.extend_from_slice(&(lib.functions.len() as u32).to_le_bytes());
    out.extend_from_slice(&fl);
    out.extend_from_slice(&ext);
    out.extend_from_slice(&publ);
    out.extend_from_slice(&priv_);
    out.extend_from_slice(&bc);
    out
}

fn rd<const N: usize>(b: &[u8], o: usize) -> Result<[u8; N], String> {
    b.get(o..o + N)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| format!("truncated at {o}"))
}
fn u16_at(b: &[u8], o: usize) -> Result<u16, String> {
    Ok(u16::from_le_bytes(rd(b, o)?))
}
fn u32_at(b: &[u8], o: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(rd(b, o)?))
}
fn u64_at(b: &[u8], o: usize) -> Result<usize, String> {
    usize::try_from(u64::from_le_bytes(rd(b, o)?)).map_err(|e| e.to_string())
}

fn get_tags(b: &[u8], mut o: usize) -> Result<(Vec<Tag>, usize), String> {
    let mut out = Vec::new();
    loop {
        let n: [u8; 4] = rd(b, o)?;
        if &n == b"ENDT" {
            return Ok((out, o + 4));
        }
        let len = u16_at(b, o + 4)? as usize;
        let v = b.get(o + 6..o + 6 + len).ok_or("tag past end")?.to_vec();
        out.push((n, v));
        o += 6 + len;
    }
}

/// Reads a library written by `metallib` (or [`write`]).
pub fn parse(b: &[u8]) -> Result<Library, String> {
    if b.get(..4) != Some(b"MTLB") {
        return Err("not a metallib".into());
    }
    let header = Header {
        platform: u16_at(b, 4)?,
        version: (u16_at(b, 6)?, u16_at(b, 8)?),
        lib_type: b[10],
        os: b[11],
        os_version: (u16_at(b, 12)?, u16_at(b, 14)?),
    };
    let fl = u64_at(b, 0x18)?;
    let fl_size = u64_at(b, 0x20)?;
    let publ = u64_at(b, 0x28)?;
    let priv_ = u64_at(b, 0x38)?;
    let bc = u64_at(b, 0x48)?;
    let n = u32_at(b, fl)? as usize;
    let mut o = fl + 4;
    let mut functions = Vec::with_capacity(n);
    for _ in 0..n {
        let size = u32_at(b, o)? as usize;
        let (tags, _) = get_tags(b, o + 4)?;
        o += size;
        let find = |k: &[u8; 4]| tags.iter().find(|t| &t.0 == k).map(|t| t.1.clone());
        let offt = find(b"OFFT").ok_or("no OFFT")?;
        let mdsz = find(b"MDSZ").ok_or("no MDSZ")?;
        let (po, vo, bo) = (u64_at(&offt, 0)?, u64_at(&offt, 8)?, u64_at(&offt, 16)?);
        let len = u64_at(&mdsz, 0)?;
        let (public, _) = get_tags(b, publ + po + 4)?;
        let (private, _) = get_tags(b, priv_ + vo + 4)?;
        let bitcode = b
            .get(bc + bo..bc + bo + len)
            .ok_or("bitcode past end")?
            .to_vec();
        functions.push(Function {
            tags,
            public,
            private,
            bitcode,
        });
    }
    if o != fl + 4 + fl_size {
        return Err("function list size mismatch".into());
    }
    let (extension, _) = get_tags(b, o)?;
    Ok(Library {
        header,
        functions,
        extension,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_from_triple() {
        let m = Header::from_triple("air64_v29-apple-macosx27.2.0").expect("mac");
        assert_eq!(
            (m.platform, m.version, m.os, m.os_version),
            (0x8001, (2, 9), 0x81, (27, 2))
        );
        let m26 = Header::from_triple("air64_v28-apple-macosx26.0.0").expect("mac 26");
        assert_eq!((m26.version, m26.os_version), ((2, 9), (26, 0)));
        let i = Header::from_triple("air64_v27-apple-ios18.0.0").expect("ios");
        assert_eq!(
            (i.platform, i.version, i.os, i.os_version),
            (1, (2, 8), 0x82, (18, 0))
        );
    }

    #[test]
    fn write_parse_round_trip() {
        let mut f = Function::new(
            "main0",
            1,
            [2, 9, 4, 1],
            vec![0xde, 0xc0, 0x17, 0x0b, 1, 2, 3, 4],
        );
        f.public.push((*b"CNST", vec![0, 0]));
        let lib = Library {
            header: Header::from_triple("air64_v29-apple-macosx27.2.0").expect("mac"),
            functions: vec![f],
            extension: Vec::new(),
        };
        let b = write(&lib);
        let back = parse(&b).expect("parses");
        assert_eq!(back.functions[0].name(), b"main0");
        assert_eq!(write(&back), b);
        assert_eq!(
            u64::from_le_bytes(b[0x10..0x18].try_into().expect("8")),
            b.len() as u64
        );
    }
}
