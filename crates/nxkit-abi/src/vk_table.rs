//! The shader table of a Vulkan game, `vk_shaders.tsv`: which Metal function holds a SPIR-V module and where its
//! descriptors go.
//!
//! The build translates each SPIR-V module of the game to a Metal function `vk_<key>` (the key is the first 8 bytes of
//! the SHA-256 of the module's bytes, as the game hands them to `vkCreateShaderModule`) and writes one row per module
//! with [`write`]. The host reads the table with [`parse`], hashes each module the game creates and finds its row.
//!
//! The text has the rows of the old engine's table (`dm/gfx/tools/vk_shaders.py`). Lines that start with `#` are
//! comments. A row has four fields, separated by tabs:
//!
//! ```text
//! key     stage   resources                           source
//! 0fcca318097ae0a2  vert  u:1.47=0                    /Content/EffectsForgeSwitch/AlphaMask.vert
//! 2e8ca24d37836637  frag  s:0.24=0 t:1.25=0 u:1.49=0  /Content/EffectsForgeSwitch/AddColorMultiplyAndLerpYRB.frag
//! ```
//!
//! The key is 16 lower-case hex digits. The stage is `vert`, `frag` or `comp:x,y,z` (the threadgroup size of a kernel,
//! which Metal takes at dispatch). The resources are separated by spaces, each `kind:set.binding=slot`, with `*count`
//! after the slot for an array of `count` descriptors at consecutive slots. Only the resources that the function takes
//! are listed. The source is where the build found the module, for messages only.

use std::collections::BTreeMap;
use std::fmt;

/// The file name of the table, in the folder `vk` of the shader output.
pub const FILE: &str = "vk_shaders.tsv";

/// The file name of the Metal library with the functions of the table, beside it.
pub const LIBRARY: &str = "vk_shaders.metallib";

/// The first line of a table that [`write`] gives, without its `# `.
pub const HEAD: &str = "Vulkan shaders: key = sha256(SPIR-V)[:16], Metal function vk_<key>; stage vert/frag/comp:x,y,z; resources kind:set.binding=slot[*count] (u uniform buffer, b storage buffer, t image, s sampler, c combined, p push constants)";

/// The descriptor sets a resource can name: 0 to 7.
pub const SETS: u32 = 8;

/// The bindings a resource can name: 0 to 127.
pub const BINDINGS: u32 = 128;

/// The largest count of an array.
pub const MAX_COUNT: u32 = 4096;

/// The Metal buffer slots of a function: 0 to 30.
pub const BUFFER_SLOTS: u32 = 31;

/// The Metal texture slots of a function: 0 to 30.
pub const TEXTURE_SLOTS: u32 = 31;

/// The Metal sampler slots of a function: 0 to 15.
pub const SAMPLER_SLOTS: u32 = 16;

/// The 8 bytes that name a SPIR-V module.
pub type Key = [u8; 8];

/// The stage of a module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    /// A vertex shader.
    Vert,
    /// A fragment shader.
    Frag,
    /// A compute shader with its threadgroup size (x, y, z), each at least 1.
    Comp([u32; 3]),
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stage::Vert => f.write_str("vert"),
            Stage::Frag => f.write_str("frag"),
            Stage::Comp([x, y, z]) => write!(f, "comp:{x},{y},{z}"),
        }
    }
}

impl Stage {
    /// Reads the stage field, or `None` when it is not one.
    pub fn parse(s: &str) -> Option<Stage> {
        match s {
            "vert" => Some(Stage::Vert),
            "frag" => Some(Stage::Frag),
            _ => {
                let mut it = s.strip_prefix("comp:")?.split(',');
                let mut size = [0u32; 3];
                for v in &mut size {
                    *v = number(it.next()?).filter(|&n| n > 0)?;
                }
                it.next().is_none().then_some(Stage::Comp(size))
            }
        }
    }
}

/// The kind of a resource: which Vulkan descriptor it is and which Metal slots it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// `u`: a uniform buffer, at a buffer slot.
    Uniform,
    /// `b`: a storage buffer, at a buffer slot.
    Storage,
    /// `t`: a sampled or storage image, at a texture slot.
    Texture,
    /// `s`: a sampler, at a sampler slot.
    Sampler,
    /// `c`: a combined image sampler, at the texture slot and the sampler slot of the same number.
    Combined,
    /// `p`: the push-constant block, at a buffer slot. Its set and binding are 0 and its count is 1.
    Push,
}

impl Kind {
    /// The letter of the kind in the table.
    pub const fn letter(self) -> char {
        match self {
            Kind::Uniform => 'u',
            Kind::Storage => 'b',
            Kind::Texture => 't',
            Kind::Sampler => 's',
            Kind::Combined => 'c',
            Kind::Push => 'p',
        }
    }

    /// The kind of a letter, or `None` when the letter is not one.
    pub const fn from_letter(c: char) -> Option<Kind> {
        Some(match c {
            'u' => Kind::Uniform,
            'b' => Kind::Storage,
            't' => Kind::Texture,
            's' => Kind::Sampler,
            'c' => Kind::Combined,
            'p' => Kind::Push,
            _ => return None,
        })
    }

    /// Whether the kind takes buffer slots.
    pub const fn is_buffer(self) -> bool {
        matches!(self, Kind::Uniform | Kind::Storage | Kind::Push)
    }

    /// Whether the kind takes texture slots.
    pub const fn is_texture(self) -> bool {
        matches!(self, Kind::Texture | Kind::Combined)
    }

    /// Whether the kind takes sampler slots.
    pub const fn is_sampler(self) -> bool {
        matches!(self, Kind::Sampler | Kind::Combined)
    }

    /// The number of Metal slots of the kind's class (for a combined image sampler, of the sampler's, the smaller one).
    pub const fn slots(self) -> u32 {
        if self.is_sampler() {
            SAMPLER_SLOTS
        } else if self.is_texture() {
            TEXTURE_SLOTS
        } else {
            BUFFER_SLOTS
        }
    }
}

/// One descriptor binding of a function and its first Metal slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Resource {
    /// What the descriptor is.
    pub kind: Kind,
    /// The descriptor set in the SPIR-V.
    pub set: u32,
    /// The binding in the SPIR-V.
    pub binding: u32,
    /// The first Metal slot.
    pub slot: u32,
    /// The number of descriptors, more than 1 for an array. They take the slots from `slot` on.
    pub count: u32,
}

impl fmt::Display for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let k = self.kind.letter();
        write!(f, "{k}:{}.{}={}", self.set, self.binding, self.slot)?;
        if self.count != 1 {
            write!(f, "*{}", self.count)?;
        }
        Ok(())
    }
}

impl Resource {
    /// Reads a token `kind:set.binding=slot[*count]`, or `None` when it is not one or out of the limits: set below
    /// [`SETS`], binding below [`BINDINGS`], count from 1 to [`MAX_COUNT`], all slots inside the class, and set,
    /// binding and count 0, 0 and 1 for the push constants.
    pub fn parse(t: &str) -> Option<Resource> {
        let mut ch = t.chars();
        let kind = Kind::from_letter(ch.next()?)?;
        let rest = ch.as_str().strip_prefix(':')?;
        let (set, rest) = rest.split_once('.')?;
        let (binding, rest) = rest.split_once('=')?;
        let (slot, count) = match rest.split_once('*') {
            Some((s, n)) => (s, number(n)?),
            None => (rest, 1),
        };
        let r = Resource {
            kind,
            set: number(set)?,
            binding: number(binding)?,
            slot: number(slot)?,
            count,
        };
        r.is_valid().then_some(r)
    }

    /// Whether the resource is inside the limits that [`Resource::parse`] names.
    pub fn is_valid(&self) -> bool {
        let push_ok = self.kind != Kind::Push || (self.set, self.binding, self.count) == (0, 0, 1);
        self.set < SETS
            && self.binding < BINDINGS
            && (1..=MAX_COUNT).contains(&self.count)
            && self.slot < self.kind.slots()
            && self.count <= self.kind.slots() - self.slot
            && push_ok
    }
}

/// One row: a module, its stage and its resources.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The first 8 bytes of the SHA-256 of the module.
    pub key: Key,
    /// The stage.
    pub stage: Stage,
    /// The resources the function takes, in the order of the table.
    pub resources: Vec<Resource>,
    /// Where the build found the module. It has no tab and no line break.
    pub source: String,
}

/// A line of a table that [`parse`] cannot read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// The line number, from 1.
    pub line: usize,
    /// What is wrong.
    pub what: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{FILE} line {}: {}", self.line, self.what)
    }
}

impl std::error::Error for Error {}

/// A decimal number without a sign, as the table writes it.
fn number(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// The 16 lower-case hex digits of a key.
pub fn key_hex(key: &Key) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

/// The key of 16 hex digits (either case), or `None` when `s` is not one.
pub fn parse_key(s: &str) -> Option<Key> {
    let b = s.as_bytes();
    if b.len() != 16 {
        return None;
    }
    let digit = |c: u8| char::from(c).to_digit(16).map(|d| d as u8);
    let mut key = [0u8; 8];
    for (k, pair) in key.iter_mut().zip(b.as_chunks::<2>().0) {
        *k = (digit(pair[0])? << 4) | digit(pair[1])?;
    }
    Some(key)
}

/// The name of the Metal function of a key: `vk_<key hex>`.
pub fn function(key: &Key) -> String {
    format!("vk_{}", key_hex(key))
}

/// The name of the dual-source variant of a fragment function: `vk_<key hex>_d`. The build writes it for a fragment
/// shader with outputs at locations 0 and 1, which become the two sources of blend attachment 0. The host takes it for a
/// pipeline whose blend factors read the second source.
pub fn dual_function(key: &Key) -> String {
    format!("vk_{}_d", key_hex(key))
}

fn row(line: &str) -> Result<Row, String> {
    let f: Vec<&str> = line.split('\t').collect();
    let [key, stage, res, source] = f[..] else {
        return Err(format!("{} fields (4 expected)", f.len()));
    };
    let key = parse_key(key).ok_or_else(|| format!("key {key:?}"))?;
    let stage = Stage::parse(stage).ok_or_else(|| format!("stage {stage:?}"))?;
    let resources = res
        .split(' ')
        .filter(|t| !t.is_empty())
        .map(|t| Resource::parse(t).ok_or_else(|| format!("resource {t:?}")))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Row {
        key,
        stage,
        resources,
        source: source.to_string(),
    })
}

/// Reads a table. Comment lines (`#`) and empty lines are skipped, and a line may end in `\r\n`. The rows come back
/// sorted by key, so a reader can search them with `binary_search_by_key`. A malformed row or a key that is in two rows
/// is an error.
pub fn parse(text: &str) -> Result<Vec<Row>, Error> {
    let mut rows: Vec<(usize, Row)> = Vec::new();
    for (k, line) in text.lines().enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let r = row(line).map_err(|what| Error { line: k + 1, what })?;
        rows.push((k + 1, r));
    }
    rows.sort_by_key(|(_, r)| r.key);
    if let Some(w) = rows.windows(2).find(|w| w[0].1.key == w[1].1.key) {
        return Err(Error {
            line: w[1].0.max(w[0].0),
            what: format!("key {} is in two rows", key_hex(&w[1].1.key)),
        });
    }
    Ok(rows.into_iter().map(|(_, r)| r).collect())
}

/// Writes a table: `# HEAD`, then each line of `header` as a comment (`# ` in front), then one row per key in key order.
/// Of two rows with one key the first wins. A tab or line break in a source becomes a space.
pub fn write(rows: &[Row], header: &str) -> String {
    let mut sorted: BTreeMap<Key, &Row> = BTreeMap::new();
    for r in rows {
        sorted.entry(r.key).or_insert(r);
    }
    let mut out = format!("# {HEAD}\n");
    for l in header.lines() {
        out += &format!("# {l}\n");
    }
    for r in sorted.into_values() {
        let res: Vec<String> = r.resources.iter().map(Resource::to_string).collect();
        let source = r.source.replace(['\t', '\n', '\r'], " ");
        out += &format!(
            "{}\t{}\t{}\t{source}\n",
            key_hex(&r.key),
            r.stage,
            res.join(" ")
        );
    }
    out
}

#[cfg(test)]
#[path = "vk_table_tests.rs"]
mod tests;
