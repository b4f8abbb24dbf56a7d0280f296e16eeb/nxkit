//! Console variables of a title that the build sets through files of the game's RomFS.
//!
//! A title lists the variables as data ([`ConsoleVar`]). Its family knows where the engine reads them from and turns
//! them into new RomFS files ([`BuildHooks::console_var_files`](crate::BuildHooks::console_var_files)). The family
//! reads the game's own files through [`RomfsRead`], and the build writes what it returns.

/// One console variable and the value the build sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConsoleVar {
    /// The name of the variable (`r.ScreenPercentage`).
    pub name: &'static str,
    /// The value as text (`50`).
    pub value: &'static str,
}

/// One entry of a RomFS directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RomfsEntry {
    /// The name, without the directory.
    pub name: String,
    /// Whether it is a directory.
    pub dir: bool,
}

/// Read access to the files of a game's RomFS. Paths are relative to the RomFS root and use `/`. A leading `/` is
/// accepted.
pub trait RomfsRead {
    /// The entries of the directory `dir` (`""` is the root). `None` when there is no such directory.
    fn list(&self, dir: &str) -> Result<Option<Vec<RomfsEntry>>, String>;
    /// The size of the file at `path`. `None` when there is no such file.
    fn len(&self, path: &str) -> Result<Option<u64>, String>;
    /// `len` bytes of the file at `path`, starting at `offset`. A range outside the file is an error.
    fn read_range(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, String>;
}

/// The files that set console variables: path from the RomFS root and bytes.
pub type ConsoleFiles = Vec<(String, Vec<u8>)>;

/// The variables of all layers as they apply: a later layer's value for a name replaces an earlier one's. The order of
/// first appearance stays.
pub fn merge_console_vars(vars: impl IntoIterator<Item = ConsoleVar>) -> Vec<ConsoleVar> {
    let mut out: Vec<ConsoleVar> = Vec::new();
    for v in vars {
        match out.iter_mut().find(|o| o.name == v.name) {
            Some(o) => o.value = v.value,
            None => out.push(v),
        }
    }
    out
}
