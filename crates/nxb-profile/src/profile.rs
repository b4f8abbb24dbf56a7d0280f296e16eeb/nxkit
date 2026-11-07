//! The typed profile (design 2.11): `[computed]` (never committed), `[declared]` (committed, few keys, each with a proof)
//! and `[rules]` (bindings for the rewriter and the host). Keys this version does not know stay in `extra` and survive a
//! round trip, so an older tool does not drop what a newer one wrote.

use crate::toml::{self, ParseError, Table, Value};
use std::collections::BTreeMap;

/// Keys a capability check decides (design 2.11 and a2 gaps 9, 13, 14): they are the default behaviour or a device
/// property, never a profile key.
pub const CAPABILITY_KEYS: &[&str] = &[
    "depth_mode",
    "tex_flush_upload",
    "tex_dirty_upload",
    "render_cond",
    "vertex_stream_null_bind",
    "gpu_timestamps",
    "alloc_big_align",
    "mem_map_limit",
    "pso_mtl4",
    "enc_thread",
    "tex_bc_cpu",
    "present_snap",
];

/// Declared keys that override a capability and so need a [`Proof`].
pub const PROVEN_KEYS: &[&str] = &["op_mode", "render_scale", "present", "fps_rules"];

/// A profile problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileError {
    /// The TOML text is wrong.
    Parse(ParseError),
    /// A key has the wrong type or value (`section.key: what`).
    Shape(String),
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileError::Parse(e) => write!(f, "profile: {e}"),
            ProfileError::Shape(s) => write!(f, "profile: {s}"),
        }
    }
}

impl std::error::Error for ProfileError {}

impl From<ParseError> for ProfileError {
    fn from(e: ParseError) -> Self {
        ProfileError::Parse(e)
    }
}

type R<T> = Result<T, ProfileError>;

/// What the host does with a finished frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Present {
    /// The game's own pacing.
    Game,
    /// The display's refresh.
    Display,
}

/// Facts read from the title's files. Never committed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Computed {
    /// NPDM address space type.
    pub address_space: Option<String>,
    /// Pool size in bytes.
    pub pool_size: Option<u64>,
    /// Sizes of the modules in bytes.
    pub module_sizes: Vec<u64>,
    /// Memory the title uses, in bytes.
    pub mem_used: Option<u64>,
    /// Shader groups that were guessed.
    pub shader_groups: Vec<String>,
    /// Pad layout from the NACP.
    pub pad_layout: Option<String>,
    /// Keys of a newer version.
    pub extra: Table,
}

/// The capability a declared key overrides and the test that proved it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proof {
    /// The capability.
    pub capability: String,
    /// The test.
    pub test: String,
}

/// Committed choices.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Declared {
    /// Title.
    pub title: Option<String>,
    /// Display name.
    pub display_name: Option<String>,
    /// Operation mode preference (`docked`, `handheld`).
    pub op_mode: Option<String>,
    /// The known render-scale verdict.
    pub render_scale: Option<String>,
    /// Present policy.
    pub present: Option<Present>,
    /// Frame rate rules, by name.
    pub fps_rules: Vec<String>,
    /// Proofs by declared key (see [`PROVEN_KEYS`]).
    pub proof: BTreeMap<String, Proof>,
    /// Keys of a newer version.
    pub extra: Table,
}

/// How the engine limits its frame rate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameLimit {
    /// `cvar`, `memory`, ...
    pub kind: String,
    /// The variable.
    pub name: String,
}

/// One byte-level cut of the rewriter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cut {
    /// What it binds to (`game`).
    pub bind: String,
    /// The fingerprint of the place.
    pub fingerprint: String,
    /// The bytes (hex).
    pub bytes: String,
    /// Why.
    pub why: String,
}

/// Bindings for the rewriter and the host.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rules {
    /// The engine (`unreal`).
    pub engine: Option<String>,
    /// The frame limit.
    pub frame_limit: Option<FrameLimit>,
    /// The cuts.
    pub cuts: Vec<Cut>,
    /// Keys of a newer version.
    pub extra: Table,
}

/// A per-game profile.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profile {
    /// The title id (16 hex digits).
    pub title_id: Option<String>,
    /// Computed.
    pub computed: Computed,
    /// Declared.
    pub declared: Declared,
    /// Rules.
    pub rules: Rules,
}

/// Takes typed keys out of a table, naming `section.key` in errors, what is left is the `extra` table.
struct Take<'a> {
    t: Table,
    at: &'a str,
}

impl Take<'_> {
    fn bad<T>(&self, key: &str, want: &str) -> R<T> {
        Err(ProfileError::Shape(format!(
            "{}.{key}: {want} expected",
            self.at
        )))
    }
    fn str(&mut self, key: &str) -> R<Option<String>> {
        match self.t.remove(key) {
            None => Ok(None),
            Some(Value::Str(s)) => Ok(Some(s)),
            Some(_) => self.bad(key, "a string"),
        }
    }
    fn need(&mut self, key: &str) -> R<String> {
        match self.str(key)? {
            Some(s) => Ok(s),
            None => self.bad(key, "a string (missing)"),
        }
    }
    fn u64(&mut self, key: &str) -> R<Option<u64>> {
        match self.t.remove(key) {
            None => Ok(None),
            Some(Value::Int(i)) if i >= 0 => Ok(Some(i as u64)),
            Some(_) => self.bad(key, "a non-negative integer"),
        }
    }
    fn list<T>(&mut self, key: &str, f: impl Fn(Value) -> Option<T>) -> R<Vec<T>> {
        match self.t.remove(key) {
            None => Ok(Vec::new()),
            Some(Value::Array(a)) => a
                .into_iter()
                .map(|v| f(v).ok_or(()))
                .collect::<Result<_, _>>()
                .or_else(|_| self.bad(key, "an array of one type")),
            Some(_) => self.bad(key, "an array"),
        }
    }
    fn table(&mut self, key: &str) -> R<Option<Table>> {
        match self.t.remove(key) {
            None => Ok(None),
            Some(Value::Table(t)) => Ok(Some(t)),
            Some(_) => self.bad(key, "a table"),
        }
    }
}

fn section(root: &mut Table, name: &'static str) -> R<Table> {
    match root.remove(name) {
        None => Ok(Table::new()),
        Some(Value::Table(t)) => Ok(t),
        Some(_) => Err(ProfileError::Shape(format!("{name}: a table expected"))),
    }
}

fn put(t: &mut Table, k: &str, v: Option<Value>) {
    if let Some(v) = v {
        t.insert(k.into(), v);
    }
}

fn s(v: &Option<String>) -> Option<Value> {
    v.clone().map(Value::Str)
}

fn n(v: Option<u64>) -> Option<Value> {
    v.map(|x| Value::Int(x as i64))
}

fn strs(v: &[String]) -> Option<Value> {
    (!v.is_empty()).then(|| Value::Array(v.iter().cloned().map(Value::Str).collect()))
}

impl Profile {
    /// Read a profile from TOML text.
    pub fn from_toml(text: &str) -> R<Profile> {
        let mut root = toml::parse(text)?;
        let mut top = Take {
            t: Table::new(),
            at: "",
        };
        let title_id = match root.remove("title_id") {
            None => None,
            Some(Value::Str(x)) => Some(x),
            Some(_) => return top.bad("title_id", "a string"),
        };
        top.t = std::mem::take(&mut root);
        let mut c = Take {
            t: section(&mut top.t, "computed")?,
            at: "computed",
        };
        let computed = Computed {
            address_space: c.str("address_space")?,
            pool_size: c.u64("pool_size")?,
            module_sizes: c.list("module_sizes", |v| match v {
                Value::Int(i) if i >= 0 => Some(i as u64),
                _ => None,
            })?,
            mem_used: c.u64("mem_used")?,
            shader_groups: c.list("shader_groups", |v| match v {
                Value::Str(s) => Some(s),
                _ => None,
            })?,
            pad_layout: c.str("pad_layout")?,
            extra: std::mem::take(&mut c.t),
        };
        let mut d = Take {
            t: section(&mut top.t, "declared")?,
            at: "declared",
        };
        let present = match d.str("present")?.as_deref() {
            None => None,
            Some("game") => Some(Present::Game),
            Some("display") => Some(Present::Display),
            Some(_) => return d.bad("present", "`game` or `display`"),
        };
        let mut proof = BTreeMap::new();
        for (k, v) in d.table("proof")?.unwrap_or_default() {
            let Value::Table(t) = v else {
                return d.bad("proof", "tables");
            };
            let mut p = Take {
                t,
                at: "declared.proof",
            };
            proof.insert(
                k,
                Proof {
                    capability: p.need("capability")?,
                    test: p.need("test")?,
                },
            );
        }
        let declared = Declared {
            title: d.str("title")?,
            display_name: d.str("display_name")?,
            op_mode: d.str("op_mode")?,
            render_scale: d.str("render_scale")?,
            present,
            fps_rules: d.list("fps_rules", |v| match v {
                Value::Str(s) => Some(s),
                _ => None,
            })?,
            proof,
            extra: std::mem::take(&mut d.t),
        };
        let mut r = Take {
            t: section(&mut top.t, "rules")?,
            at: "rules",
        };
        let frame_limit = match r.table("frame_limit")? {
            None => None,
            Some(t) => {
                let mut f = Take {
                    t,
                    at: "rules.frame_limit",
                };
                Some(FrameLimit {
                    kind: f.need("kind")?,
                    name: f.need("name")?,
                })
            }
        };
        let mut cuts = Vec::new();
        for v in r.list("cuts", Some)? {
            let Value::Table(t) = v else {
                return r.bad("cuts", "tables");
            };
            let mut c = Take {
                t,
                at: "rules.cuts",
            };
            cuts.push(Cut {
                bind: c.need("bind")?,
                fingerprint: c.need("fingerprint")?,
                bytes: c.need("bytes")?,
                why: c.need("why")?,
            });
        }
        let rules = Rules {
            engine: r.str("engine")?,
            frame_limit,
            cuts,
            extra: std::mem::take(&mut r.t),
        };
        let mut p = Profile {
            title_id,
            computed,
            declared,
            rules,
        };
        // sections of a newer version are kept whole in `computed.extra` under their own name
        for (k, v) in top.t {
            p.computed.extra.insert(format!("@{k}"), v);
        }
        Ok(p)
    }

    /// The value tree of the profile (what [`Profile::to_toml`] writes).
    pub fn to_table(&self) -> Table {
        let mut root = Table::new();
        put(&mut root, "title_id", s(&self.title_id));
        let c = &self.computed;
        let mut ct = c.extra.clone();
        let tops: Vec<(String, Value)> = ct
            .iter()
            .filter(|(k, _)| k.starts_with('@'))
            .map(|(k, v)| (k[1..].to_string(), v.clone()))
            .collect();
        ct.retain(|k, _| !k.starts_with('@'));
        root.extend(tops);
        put(&mut ct, "address_space", s(&c.address_space));
        put(&mut ct, "pool_size", n(c.pool_size));
        put(
            &mut ct,
            "module_sizes",
            (!c.module_sizes.is_empty()).then(|| {
                Value::Array(
                    c.module_sizes
                        .iter()
                        .map(|x| Value::Int(*x as i64))
                        .collect(),
                )
            }),
        );
        put(&mut ct, "mem_used", n(c.mem_used));
        put(&mut ct, "shader_groups", strs(&c.shader_groups));
        put(&mut ct, "pad_layout", s(&c.pad_layout));
        let d = &self.declared;
        let mut dt = d.extra.clone();
        put(&mut dt, "title", s(&d.title));
        put(&mut dt, "display_name", s(&d.display_name));
        put(&mut dt, "op_mode", s(&d.op_mode));
        put(&mut dt, "render_scale", s(&d.render_scale));
        put(
            &mut dt,
            "present",
            d.present.map(|p| {
                Value::Str(
                    if p == Present::Game {
                        "game"
                    } else {
                        "display"
                    }
                    .into(),
                )
            }),
        );
        put(&mut dt, "fps_rules", strs(&d.fps_rules));
        if !d.proof.is_empty() {
            let pt = d.proof.iter().map(|(k, p)| {
                let t = Table::from([
                    ("capability".to_string(), Value::Str(p.capability.clone())),
                    ("test".to_string(), Value::Str(p.test.clone())),
                ]);
                (k.clone(), Value::Table(t))
            });
            dt.insert("proof".into(), Value::Table(pt.collect()));
        }
        let r = &self.rules;
        let mut rt = r.extra.clone();
        put(&mut rt, "engine", s(&r.engine));
        if let Some(f) = &r.frame_limit {
            let t = Table::from([
                ("kind".to_string(), Value::Str(f.kind.clone())),
                ("name".to_string(), Value::Str(f.name.clone())),
            ]);
            rt.insert("frame_limit".into(), Value::Table(t));
        }
        if !r.cuts.is_empty() {
            let cuts = r.cuts.iter().map(|c| {
                let f = |k: &str, v: &String| (k.to_string(), Value::Str(v.clone()));
                Value::Table(Table::from([
                    f("bind", &c.bind),
                    f("fingerprint", &c.fingerprint),
                    f("bytes", &c.bytes),
                    f("why", &c.why),
                ]))
            });
            rt.insert("cuts".into(), Value::Array(cuts.collect()));
        }
        for (name, t) in [("computed", ct), ("declared", dt), ("rules", rt)] {
            if !t.is_empty() {
                root.insert(name.into(), Value::Table(t));
            }
        }
        root
    }

    /// Write the profile as TOML.
    pub fn to_toml(&self) -> String {
        toml::write(&self.to_table())
    }

    /// The text of the committed part only (`declared` and `rules`): what goes into the repository.
    pub fn to_committed_toml(&self) -> String {
        let p = Profile {
            computed: Computed::default(),
            ..self.clone()
        };
        p.to_toml()
    }

    /// A value by dotted path (`declared.title`, `rules.frame_limit.name`), the one interface for code that reads keys by
    /// name (the `game.py` header and settings code).
    pub fn get(&self, path: &str) -> Option<Value> {
        let mut cur = Value::Table(self.to_table());
        for part in path.split('.') {
            cur = match cur {
                Value::Table(mut t) => t.remove(part)?,
                _ => return None,
            };
        }
        Some(cur)
    }

    /// [`Profile::get`] as a string.
    pub fn get_str(&self, path: &str) -> Option<String> {
        match self.get(path)? {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The rules of section 2.11. Returns every problem found, empty when the profile is fine:
    /// a declared override without its proof, a key that a capability check decides, a cut without a reason.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        let d = &self.declared;
        let has = [
            ("op_mode", d.op_mode.is_some()),
            ("render_scale", d.render_scale.is_some()),
            ("present", d.present.is_some()),
            ("fps_rules", !d.fps_rules.is_empty()),
        ];
        for (k, set) in has {
            if set
                && !d
                    .proof
                    .get(k)
                    .is_some_and(|p| !p.capability.is_empty() && !p.test.is_empty())
            {
                out.push(format!(
                    "declared.{k} needs [declared.proof.{k}] with a capability and a test"
                ));
            }
        }
        for k in d.proof.keys() {
            if !PROVEN_KEYS.contains(&k.as_str()) {
                out.push(format!(
                    "declared.proof.{k}: not a key that can carry a proof"
                ));
            }
        }
        for (sec, t) in [("declared", &d.extra), ("rules", &self.rules.extra)] {
            for k in t.keys().filter(|k| CAPABILITY_KEYS.contains(&k.as_str())) {
                out.push(format!(
                    "{sec}.{k} is decided by a capability check, it is not a key"
                ));
            }
        }
        for (i, c) in self.rules.cuts.iter().enumerate() {
            if c.why.trim().is_empty() {
                out.push(format!("rules.cuts[{i}] has no reason"));
            }
        }
        out
    }

    /// A provisional profile is the computed section alone and must still boot.
    pub fn is_provisional(&self) -> bool {
        self.declared == Declared::default() && self.rules == Rules::default()
    }
}
