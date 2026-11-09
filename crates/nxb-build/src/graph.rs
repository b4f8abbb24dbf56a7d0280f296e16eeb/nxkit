//! What a build knows once the inputs are found: the title, the packages, the folders and the shader step's place.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Everything the stages of a build read.
#[derive(Debug)]
pub struct Build {
    /// The game's title ID.
    pub title: String,
    /// Base NSP (or the NUP file).
    pub base: PathBuf,
    /// Updates (version, path).
    pub updates: Vec<(u32, PathBuf)>,
    /// The packed version (`v1114112`).
    pub package: String,
    /// Work folder.
    pub work: PathBuf,
    /// The profile folder `<work>/profile` and its `game.tsv` keys.
    pub profile: (PathBuf, BTreeMap<String, String>),
    /// The shader libraries (`b-<chain>`).
    pub shaders: PathBuf,
    /// GPU architectures of the pipeline archive (empty: none).
    pub gpus: Vec<String>,
    /// What the game is read from.
    pub src: crate::inputs::Inputs,
}
