//! The index of the pipeline archive shards: which shard file holds which pipeline.
//!
//! The build compiles the pipelines in batches (`nxb-archive`). Instead of merging the batch archives into one file of
//! several GB, which `MTL4Archive` reads whole into memory, it ships each batch archive as a shard file and this
//! index. The host opens a shard when the first pipeline that the index maps to it is asked for.
//!
//! A pipeline is identified by what the host has when it asks: the function names of its stages and the file stems of
//! the libraries they are in (`<key>_b`, the name of `<key>_b.metallib`). Nothing of the colour state is in the
//! identity: the build archives every pair with unspecialized state and Metal matches the specialized descriptor of
//! the host to it.
//!
//! The file is `index.nxpi` in the folder `pipelines` beside the shard files. All numbers are little endian:
//!
//! ```text
//! "NXPI" u32 version(1)
//! u32 n_strings   { u32 len, bytes }*          every library stem, function name and shard file name once
//! u32 n_shards    { u32 string }*              the file names of the shards
//! u32 n_render    { vlib ventry flib fentry shard : 5 x u32 }*
//! u32 n_compute   { lib entry shard : 3 x u32 }*
//! u64 checksum                                  FNV-1a of every byte before it
//! ```

use std::collections::HashMap;

/// The folder of the shards, in the run folder of the app.
pub const SHARD_DIR: &str = "pipelines";

/// The file name of the index, in [`SHARD_DIR`].
pub const INDEX_FILE: &str = "index.nxpi";

/// The extension of a shard file.
pub const SHARD_EXT: &str = "mtl4archive";

const MAGIC: &[u8; 4] = b"NXPI";
const VERSION: u32 = 1;

/// A function of a library: the library's file stem and the entry name.
pub type Func<'a> = (&'a str, &'a str);

/// What a shard index is made of.
#[derive(Default)]
pub struct IndexBuilder {
    strings: Vec<String>,
    ids: HashMap<String, u32>,
    shards: Vec<u32>,
    render: Vec<[u32; 5]>,
    compute: Vec<[u32; 3]>,
}

impl IndexBuilder {
    /// An empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    fn string(&mut self, s: &str) -> u32 {
        if let Some(i) = self.ids.get(s) {
            return *i;
        }
        let i = self.strings.len() as u32;
        self.strings.push(s.to_string());
        self.ids.insert(s.to_string(), i);
        i
    }

    /// The shard `file` (a bare file name). Returns its number for [`IndexBuilder::render`] and
    /// [`IndexBuilder::compute`]. The same name again gives the same number.
    pub fn shard(&mut self, file: &str) -> u32 {
        let s = self.string(file);
        if let Some(i) = self.shards.iter().position(|x| *x == s) {
            return i as u32;
        }
        self.shards.push(s);
        (self.shards.len() - 1) as u32
    }

    /// A render pipeline of vertex and fragment function in `shard`.
    pub fn render(&mut self, vertex: Func<'_>, fragment: Func<'_>, shard: u32) {
        let r = [
            self.string(vertex.0),
            self.string(vertex.1),
            self.string(fragment.0),
            self.string(fragment.1),
            shard,
        ];
        self.render.push(r);
    }

    /// A compute pipeline of `function` in `shard`.
    pub fn compute(&mut self, function: Func<'_>, shard: u32) {
        let r = [self.string(function.0), self.string(function.1), shard];
        self.compute.push(r);
    }

    /// The bytes of the index file.
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Vec::new();
        o.extend_from_slice(MAGIC);
        put(&mut o, VERSION);
        put(&mut o, self.strings.len() as u32);
        for s in &self.strings {
            put(&mut o, s.len() as u32);
            o.extend_from_slice(s.as_bytes());
        }
        put(&mut o, self.shards.len() as u32);
        self.shards.iter().for_each(|s| put(&mut o, *s));
        put(&mut o, self.render.len() as u32);
        self.render.iter().flatten().for_each(|v| put(&mut o, *v));
        put(&mut o, self.compute.len() as u32);
        self.compute.iter().flatten().for_each(|v| put(&mut o, *v));
        let sum = fnv(&o);
        o.extend_from_slice(&sum.to_le_bytes());
        o
    }
}

fn put(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_le_bytes());
}

fn fnv(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325, |h, c| {
        (h ^ u64::from(*c)).wrapping_mul(0x100_0000_01b3)
    })
}

/// A cursor over the index bytes.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.0.len() < n {
            return Err("the shard index is cut short".into());
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }

    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// A count whose entries of `size` bytes must still fit in what is left, so a wrong count cannot allocate.
    fn count(&mut self, size: usize) -> Result<usize, String> {
        let n = self.u32()? as usize;
        if n.saturating_mul(size) > self.0.len() {
            return Err("the shard index counts more entries than it holds".into());
        }
        Ok(n)
    }
}

/// A shard file name is one plain file name, so a damaged index cannot name a file outside the folder.
fn plain_name(n: &str) -> bool {
    !n.is_empty() && !n.contains(['/', '\\', '\0']) && n != "." && n != ".."
}

/// The index of an app: pipeline identity to shard.
#[derive(Debug, Default)]
pub struct ShardIndex {
    files: Vec<String>,
    /// Interned library stems and function names.
    names: HashMap<String, u32>,
    render: HashMap<[u32; 4], u32>,
    compute: HashMap<[u32; 2], u32>,
}

impl ShardIndex {
    /// Reads an index file's bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let body = bytes
            .len()
            .checked_sub(8)
            .map(|n| bytes.split_at(n))
            .ok_or("the shard index is cut short")?;
        let mut sum = [0u8; 8];
        sum.copy_from_slice(body.1);
        if fnv(body.0) != u64::from_le_bytes(sum) {
            return Err("the shard index checksum does not match".into());
        }
        let mut r = Reader(body.0);
        if r.take(4)? != MAGIC {
            return Err("not a shard index".into());
        }
        let v = r.u32()?;
        if v != VERSION {
            return Err(format!(
                "shard index version {v}, this host reads {VERSION}"
            ));
        }
        let n = r.count(4)?;
        let mut strings = Vec::with_capacity(n);
        for _ in 0..n {
            let len = r.count(1)?;
            let s = std::str::from_utf8(r.take(len)?).map_err(|_| "a name is not UTF-8")?;
            strings.push(s.to_string());
        }
        let n = r.count(4)?;
        let mut files = Vec::with_capacity(n);
        for _ in 0..n {
            let i = r.u32()? as usize;
            let name = strings.get(i).ok_or("a shard names a missing string")?;
            if !plain_name(name) {
                return Err(format!("shard file name {name:?} is not a plain name"));
            }
            files.push(name.clone());
        }
        let mut ix = ShardIndex {
            files,
            ..Self::default()
        };
        // Only the strings of pipelines are interned: the file names are not looked up by function.
        let mut ids = |i: u32| -> Result<u32, String> {
            let s = strings
                .get(i as usize)
                .ok_or("a pipeline names a missing string")?;
            let next = ix.names.len() as u32;
            Ok(*ix.names.entry(s.clone()).or_insert(next))
        };
        let n = r.count(20)?;
        let mut render = Vec::with_capacity(n);
        for _ in 0..n {
            let k = [
                ids(r.u32()?)?,
                ids(r.u32()?)?,
                ids(r.u32()?)?,
                ids(r.u32()?)?,
            ];
            render.push((k, r.u32()?));
        }
        let n = r.count(12)?;
        let mut compute = Vec::with_capacity(n);
        for _ in 0..n {
            compute.push(([ids(r.u32()?)?, ids(r.u32()?)?], r.u32()?));
        }
        let shards = ix.files.len() as u32;
        for (k, s) in render {
            if s >= shards {
                return Err("a pipeline names a missing shard".into());
            }
            ix.render.entry(k).or_insert(s);
        }
        for (k, s) in compute {
            if s >= shards {
                return Err("a pipeline names a missing shard".into());
            }
            ix.compute.entry(k).or_insert(s);
        }
        Ok(ix)
    }

    fn name(&self, s: &str) -> Option<u32> {
        self.names.get(s).copied()
    }

    /// The shard (its number) of the render pipeline of these two functions. The first shard that holds it wins.
    pub fn render(&self, vertex: Func<'_>, fragment: Func<'_>) -> Option<u32> {
        let k = [
            self.name(vertex.0)?,
            self.name(vertex.1)?,
            self.name(fragment.0)?,
            self.name(fragment.1)?,
        ];
        self.render.get(&k).copied()
    }

    /// The shard (its number) of the compute pipeline of this function.
    pub fn compute(&self, function: Func<'_>) -> Option<u32> {
        let k = [self.name(function.0)?, self.name(function.1)?];
        self.compute.get(&k).copied()
    }

    /// The file name of shard `n`.
    pub fn file(&self, n: u32) -> Option<&str> {
        self.files.get(n as usize).map(String::as_str)
    }

    /// The number of shard files.
    pub fn shards(&self) -> usize {
        self.files.len()
    }

    /// The number of pipelines (render and compute) the index knows.
    pub fn pipelines(&self) -> usize {
        self.render.len() + self.compute.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut b = IndexBuilder::new();
        let (s0, s1) = (b.shard("aa.mtl4archive"), b.shard("bb.mtl4archive"));
        assert_eq!(b.shard("aa.mtl4archive"), s0);
        b.render(("v1_b", "main0"), ("f1_b", "main0"), s0);
        b.render(("v2_b", "main0"), ("f1_b", "main0"), s1);
        // the same pair in a second shard: the first one wins
        b.render(("v1_b", "main0"), ("f1_b", "main0"), s1);
        b.compute(("c1_b", "kern"), s1);
        b.encode()
    }

    #[test]
    fn the_index_round_trips() {
        let ix = ShardIndex::decode(&sample()).unwrap();
        assert_eq!((ix.shards(), ix.pipelines()), (2, 3));
        let s = ix.render(("v1_b", "main0"), ("f1_b", "main0")).unwrap();
        assert_eq!(ix.file(s), Some("aa.mtl4archive"));
        let s = ix.render(("v2_b", "main0"), ("f1_b", "main0")).unwrap();
        assert_eq!(ix.file(s), Some("bb.mtl4archive"));
        let s = ix.compute(("c1_b", "kern")).unwrap();
        assert_eq!(ix.file(s), Some("bb.mtl4archive"));
    }

    #[test]
    fn a_pipeline_is_found_by_the_pair_not_by_one_stage() {
        let ix = ShardIndex::decode(&sample()).unwrap();
        // both stages are known, the pair is not
        assert_eq!(ix.render(("v2_b", "main0"), ("v1_b", "main0")), None);
        assert_eq!(ix.render(("v1_b", "main0"), ("f9_b", "main0")), None);
        assert_eq!(ix.render(("v1_b", "other"), ("f1_b", "main0")), None);
        // a render pair is no compute pipeline
        assert_eq!(ix.compute(("v1_b", "main0")), None);
        assert_eq!(ix.file(7), None);
    }

    #[test]
    fn an_empty_index_is_valid() {
        let ix = ShardIndex::decode(&IndexBuilder::new().encode()).unwrap();
        assert_eq!((ix.shards(), ix.pipelines()), (0, 0));
    }

    #[test]
    fn damage_is_refused() {
        let good = sample();
        assert!(ShardIndex::decode(&[]).is_err());
        assert!(ShardIndex::decode(&good[..good.len() - 1]).is_err());
        let mut bad = good.clone();
        bad[20] ^= 1;
        assert!(ShardIndex::decode(&bad).is_err());
        // a wrong version with a good checksum
        let mut v = good[..good.len() - 8].to_vec();
        v[4] = 9;
        let sum = fnv(&v);
        v.extend_from_slice(&sum.to_le_bytes());
        assert!(ShardIndex::decode(&v).unwrap_err().contains("version 9"));
    }

    #[test]
    fn a_shard_name_cannot_leave_the_folder() {
        for name in ["../x", "a/b", "..", "", "a\\b"] {
            let mut b = IndexBuilder::new();
            b.shard(name);
            assert!(ShardIndex::decode(&b.encode()).is_err(), "{name:?}");
        }
    }

    #[test]
    fn a_pipeline_cannot_name_a_missing_shard() {
        let mut b = IndexBuilder::new();
        b.shard("a.mtl4archive");
        b.compute(("c_b", "k"), 5);
        assert!(ShardIndex::decode(&b.encode()).is_err());
    }

    #[test]
    fn a_huge_count_does_not_allocate() {
        let mut o = Vec::new();
        o.extend_from_slice(MAGIC);
        put(&mut o, VERSION);
        put(&mut o, u32::MAX);
        let sum = fnv(&o);
        o.extend_from_slice(&sum.to_le_bytes());
        assert!(ShardIndex::decode(&o).is_err());
    }
}
