//! The keys of the pipeline cache: the hash of what a draw asks for, as a function of plain data.
//!
//! The runtime pipeline cache (`runtime.rs`) keeps `pipelines.seen` (`record.rs`) between host builds, and that file
//! holds these keys. So a key must be the same for the same description in every host build: [`KEY_SCHEME`] says which
//! scheme the keys follow, and the tests pin one key of each kind.

use std::hash::{DefaultHasher, Hash, Hasher};

use nxkit_metal::{ColorTarget, TopologyClass, VertexDescriptor};

use super::Targets;

/// The version of the key scheme: how the keys of the pipelines are computed and what `pipelines.seen` says about
/// them. It names the folder of the runtime pipeline cache (`runtime.rs`), so a host build with the same scheme finds
/// the archives and the list of the build before it, and a host build with another scheme starts an empty cache.
///
/// Raise it when the key of a pipeline changes for the same recorded description:
///
/// - the set or the order of what [`render_key`], [`mesh_key`], [`compute_key`] or [`point_size_key`] hash changes,
///   or a key function is added or removed;
/// - a type that they hash changes its `Hash`: `Targets`, `ColorTarget`, `BlendState` or `VertexDescriptor` (or what is
///   inside them) gains or loses a field, or an enum (a format, a blend factor, a topology class) changes the order
///   or the number of its variants;
/// - the meaning of a field of `pipelines.seen` changes (what the line of an entry holds, not only its layout: the
///   version line of the file covers the layout).
///
/// The keys come from `std::collections::hash_map::DefaultHasher`. Rust does not promise its algorithm across
/// releases. If a release changes it, every key changes: the pinned tests of this module fail, and the scheme is
/// raised then.
pub(crate) const KEY_SCHEME: u32 = 1;

/// What the key of a render pipeline hashes.
pub(super) struct RenderKey<'a> {
    /// The shader key of the vertex program.
    pub vertex: &'a str,
    /// The shader key of the fragment program.
    pub fragment: Option<&'a str>,
    /// The formats of the colour slots and the sample count.
    pub targets: &'a Targets,
    /// The pipeline supports indirect command buffers.
    pub icb: bool,
    /// The colour attachments with blend state and write mask.
    pub colors: &'a [Option<ColorTarget>],
    /// The vertex layout (empty when the vertex function pulls).
    pub layout: &'a VertexDescriptor,
    /// The vertex function pulls its inputs.
    pub pull: bool,
    /// The topology class.
    pub class: TopologyClass,
    /// Alpha to coverage.
    pub alpha_to_coverage: bool,
    /// The debug fragment function `flat` is used.
    pub flat: bool,
    /// The debug fragment function `paint` is used.
    pub paint: bool,
    /// The debug fragment function `paint_dir` is used.
    pub paint_dir: bool,
}

/// The key of a render pipeline.
pub(super) fn render_key(k: &RenderKey<'_>) -> u64 {
    let mut h = DefaultHasher::new();
    (k.vertex, k.fragment, k.targets, k.icb).hash(&mut h);
    k.colors.hash(&mut h);
    k.layout.hash(&mut h);
    if k.pull {
        "pull".hash(&mut h);
    }
    k.class.hash(&mut h);
    k.alpha_to_coverage.hash(&mut h);
    k.flat.hash(&mut h);
    k.paint.hash(&mut h);
    k.paint_dir.hash(&mut h);
    h.finish()
}

/// What the key of a mesh pipeline hashes.
pub(super) struct MeshKey<'a> {
    /// The shader key of the task program.
    pub task: Option<&'a str>,
    /// The shader key of the mesh program.
    pub mesh: Option<&'a str>,
    /// The shader key of the fragment program.
    pub fragment: Option<&'a str>,
    /// The entry name of the fragment function.
    pub fragment_entry: Option<&'a str>,
    /// The formats of the colour slots and the sample count.
    pub targets: &'a Targets,
    /// The colour attachments with blend state and write mask.
    pub colors: &'a [Option<ColorTarget>],
    /// Alpha to coverage.
    pub alpha_to_coverage: bool,
}

/// The key of a mesh pipeline.
pub(super) fn mesh_key(k: &MeshKey<'_>) -> u64 {
    let mut h = DefaultHasher::new();
    (
        "mesh",
        k.task,
        k.mesh,
        k.fragment,
        k.fragment_entry,
        k.targets,
    )
        .hash(&mut h);
    k.colors.hash(&mut h);
    k.alpha_to_coverage.hash(&mut h);
    h.finish()
}

/// The key of the compute pipeline of the program with the shader key `program`.
pub(super) fn compute_key(program: &str) -> u64 {
    let mut h = DefaultHasher::new();
    ("compute", program).hash(&mut h);
    h.finish()
}

/// The key under which `fallbacks` remembers that the vertex function `entry` of the program `vertex` writes the
/// point size, so every later state of it skips the class at once. The list stores it as the vertex key of an entry.
pub(super) fn point_size_key(vertex: &str, entry: &str) -> u64 {
    let mut h = DefaultHasher::new();
    ("point-size", vertex, entry).hash(&mut h);
    h.finish()
}

/// The shader key of the program of a library stem: `<key>_b`, or `<key>_bp` for a vertex function that pulls.
#[cfg(test)]
fn program_of(stem: &str) -> &str {
    let stem = stem.strip_suffix('p').unwrap_or(stem);
    stem.strip_suffix("_b").unwrap_or(stem)
}

/// The keys that the encoder could have computed for the entry of a list, from its fields alone (tests only).
///
/// Every input of a key is in the entry: the program keys are the stems without `_b` (`_bp` marks a pulling vertex
/// function, whose layout is empty), the `Targets` are the formats of the colour targets and the sample count, and
/// the rest are fields. Not in a render entry: the debug flags (assumed off) and the topology class of an entry that
/// the topology fallback made, which is recorded as `Unspecified`. That entry is not `Point`, so the keys of `Line`
/// and `Triangle` are given. Every other entry has exactly one key.
#[cfg(test)]
pub(super) fn entry_keys(e: &super::record::Entry) -> Vec<u64> {
    use super::record::Entry;
    let targets = |colors: &[Option<ColorTarget>], samples| Targets {
        colors: colors.iter().map(|c| c.map(|c| c.format)).collect(),
        samples,
    };
    match e {
        Entry::Render(r) => {
            let t = targets(&r.colors, r.samples);
            let classes = if r.fallback && r.topology == TopologyClass::Unspecified {
                vec![TopologyClass::Line, TopologyClass::Triangle]
            } else {
                vec![r.topology]
            };
            classes
                .into_iter()
                .map(|class| {
                    render_key(&RenderKey {
                        vertex: program_of(&r.vertex.0),
                        fragment: r.fragment.as_ref().map(|f| program_of(&f.0)),
                        targets: &t,
                        icb: r.icb,
                        colors: &r.colors,
                        layout: &r.layout,
                        pull: r.vertex.0.ends_with("_bp"),
                        class,
                        alpha_to_coverage: r.a2c,
                        flat: false,
                        paint: false,
                        paint_dir: false,
                    })
                })
                .collect()
        }
        Entry::Mesh(m) => {
            let t = targets(&m.colors, m.samples);
            vec![mesh_key(&MeshKey {
                task: m.object.as_ref().map(|o| program_of(&o.0)),
                mesh: Some(program_of(&m.mesh.0)),
                fragment: m.fragment.as_ref().map(|f| program_of(&f.0)),
                fragment_entry: m.fragment.as_ref().map(|f| f.1.as_str()),
                targets: &t,
                colors: &m.colors,
                alpha_to_coverage: m.a2c,
            })]
        }
        Entry::Compute(c) => vec![compute_key(program_of(&c.function.0))],
    }
}

#[cfg(test)]
mod recorded;
#[cfg(test)]
mod tests;
