//! The input file: the mouse camera and changes to the keyboard and mouse layout, per user and per title (R63).
//!
//! Two files are read, each only if it exists, and the later one wins:
//!
//! 1. `~/Library/Application Support/nxkit/input.json`, for every title.
//! 2. `input.json` of the run folder, next to `game.json`, for this title.
//!
//! Each file has an optional top-level section and an optional `titles` object with one section per title ID. The
//! section of the running title follows the top-level one. A section looks like this:
//!
//! ```json
//! {
//!   "keyboard": true,
//!   "mouse": { "sensitivity": 1.0, "invert_y": false, "deadzone": 0.12, "capture": true },
//!   "bind": { "f": "rstick", "mouse4": "zr", "c": "none" }
//! }
//! ```
//!
//! The `bind` pairs change the PC layout of the input service, one key or mouse button at a time, in key order. The
//! names are listed in `docs/nxkit-host.md`. A file that does not parse, a key that is not known and a binding that
//! does not parse give a warning and are skipped, so a typo does not stop the game.

use std::path::{Path, PathBuf};

use nxkit_svc_input::{Config, LookConfig};

use crate::json::{self, Value};

/// The file name in both places.
const FILE: &str = "input.json";

/// What the input files set.
#[derive(Debug, Clone, PartialEq)]
pub struct InputFile {
    /// Keyboard and mouse on or off.
    pub keyboard: bool,
    /// The window holds the mouse when no controller is connected.
    pub mouse_capture: bool,
    /// The mouse camera.
    pub look: LookConfig,
    /// The binding pairs, in the order of the files.
    pub binds: Vec<(String, String)>,
}

impl Default for InputFile {
    fn default() -> Self {
        let c = Config::default();
        InputFile { keyboard: c.keyboard, mouse_capture: c.mouse_capture, look: c.look, binds: Vec::new() }
    }
}

fn number(v: &Value) -> Option<f32> {
    match v {
        Value::Int(i) => Some(*i as f32),
        Value::Float(f) => Some(*f as f32),
        _ => None,
    }
}

impl InputFile {
    /// Puts the values into the configuration of the input service.
    pub fn apply_to(&self, config: &mut Config) {
        config.keyboard = self.keyboard;
        config.mouse_capture = self.mouse_capture;
        config.look = self.look;
        config.binds.clone_from(&self.binds);
    }

    /// Reads one parsed file: the top-level section, then the section of `title_id` in `titles`. Returns the
    /// warnings.
    pub fn read(&mut self, v: &Value, title_id: &str) -> Vec<String> {
        if !matches!(v, Value::Object(_)) {
            return vec!["the file is not a JSON object".to_owned()];
        }
        let mut warnings = self.section(v, "");
        match v.get("titles") {
            None => {}
            Some(Value::Object(titles)) => {
                let own = titles.iter().find(|(id, _)| id.eq_ignore_ascii_case(title_id));
                if let Some((id, section)) = own {
                    warnings.extend(self.section(section, &format!("titles.{id}.")));
                }
            }
            Some(_) => warnings.push("titles is not an object".to_owned()),
        }
        warnings
    }

    fn section(&mut self, v: &Value, at: &str) -> Vec<String> {
        let mut warnings = Vec::new();
        let Value::Object(map) = v else {
            return vec![format!("{at} is not an object")];
        };
        for (key, value) in map {
            match (key.as_str(), value) {
                ("titles", _) if at.is_empty() => {}
                ("keyboard", Value::Bool(b)) => self.keyboard = *b,
                ("mouse", Value::Object(_)) => warnings.extend(self.mouse(value, &format!("{at}mouse."))),
                ("bind", Value::Object(pairs)) => {
                    for (k, a) in pairs {
                        match a.as_str() {
                            Some(a) => self.binds.push((k.clone(), a.to_owned())),
                            None => warnings.push(format!("{at}bind.{k} is not a string")),
                        }
                    }
                }
                _ => warnings.push(format!("{at}{key} is not known or has the wrong type")),
            }
        }
        warnings
    }

    fn mouse(&mut self, v: &Value, at: &str) -> Vec<String> {
        let mut warnings = Vec::new();
        let Value::Object(map) = v else {
            return warnings;
        };
        for (key, value) in map {
            match (key.as_str(), number(value), value.as_bool()) {
                ("sensitivity", Some(n), _) => self.look.sensitivity = n,
                ("deadzone", Some(n), _) => self.look.deadzone = n,
                ("invert_y", _, Some(b)) => self.look.invert_y = b,
                ("capture", _, Some(b)) => self.mouse_capture = b,
                _ => warnings.push(format!("{at}{key} is not known or has the wrong type")),
            }
        }
        warnings
    }
}

/// The input files in the order they are read: the one of the user, then the one of the run folder.
pub fn paths(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        out.push(PathBuf::from(home).join("Library/Application Support/nxkit").join(FILE));
    }
    out.push(root.join(FILE));
    out
}

/// Reads the input files of a title. Returns the values and the warnings, each with its file.
pub fn load(root: &Path, title_id: &str) -> (InputFile, Vec<String>) {
    let mut file = InputFile::default();
    let mut warnings = Vec::new();
    for path in paths(root) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let found = match json::parse(&text) {
            Ok(v) => file.read(&v, title_id),
            Err(e) => vec![e.to_string()],
        };
        warnings.extend(found.into_iter().map(|w| format!("{}: {w}", path.display())));
    }
    let bad = nxkit_svc_input::bind_errors(&file.binds);
    warnings.extend(bad.into_iter().map(|w| format!("bind: {w}, skipped")));
    (file, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTW: &str = "01007EF00011E000";

    fn read(text: &str) -> (InputFile, Vec<String>) {
        let mut f = InputFile::default();
        let w = match json::parse(text) {
            Ok(v) => f.read(&v, BOTW),
            Err(e) => vec![e.to_string()],
        };
        (f, w)
    }

    #[test]
    fn an_empty_file_keeps_the_defaults() {
        let (f, w) = read("{}");
        assert_eq!(f, InputFile::default());
        assert!(w.is_empty());
    }

    #[test]
    fn reads_the_mouse_and_the_bindings() {
        let (f, w) = read(
            r#"{"keyboard": true, "mouse": {"sensitivity": 2, "invert_y": true, "deadzone": 0.2, "capture": false},
                "bind": {"f": "rstick", "mouse4": "zr"}}"#,
        );
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(f.look, LookConfig { sensitivity: 2.0, invert_y: true, deadzone: 0.2 });
        assert!(!f.mouse_capture);
        assert_eq!(f.binds, vec![("f".to_owned(), "rstick".to_owned()), ("mouse4".to_owned(), "zr".to_owned())]);
    }

    #[test]
    fn the_title_section_follows_the_top_level_one() {
        let (f, w) = read(
            r#"{"mouse": {"sensitivity": 2}, "bind": {"f": "x"},
                "titles": {"01007ef00011e000": {"mouse": {"sensitivity": 3}, "bind": {"f": "y"}},
                           "0100000000010000": {"keyboard": false}}}"#,
        );
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(f.look.sensitivity, 3.0);
        assert!(f.keyboard);
        assert_eq!(f.binds.last(), Some(&("f".to_owned(), "y".to_owned())));
    }

    #[test]
    fn unknown_keys_and_wrong_types_warn() {
        let (f, w) = read(r#"{"mouse": {"speed": 1, "invert_y": 1}, "bind": {"f": 3}, "rumble": true}"#);
        assert_eq!(w.len(), 4, "{w:?}");
        assert_eq!(f, InputFile::default());
        let (_, w) = read("[1]");
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn the_values_reach_the_service_config() {
        let (f, _) = read(r#"{"keyboard": false, "bind": {"q": "zl"}}"#);
        let mut c = Config::default();
        f.apply_to(&mut c);
        assert!(!c.keyboard);
        assert_eq!(c.binds, vec![("q".to_owned(), "zl".to_owned())]);
    }

    #[test]
    fn the_run_folder_file_is_read_last() {
        let p = paths(Path::new("/run"));
        assert_eq!(p.last(), Some(&PathBuf::from("/run/input.json")));
    }

    #[test]
    fn load_reads_the_run_folder() {
        let dir = std::env::temp_dir().join(format!("nxkit-input-{}", std::process::id()));
        assert!(std::fs::create_dir_all(&dir).is_ok());
        assert!(std::fs::write(dir.join(FILE), r#"{"mouse": {"sensitivity": 4}, "bind": {"x": "jump"}}"#).is_ok());
        let (f, w) = load(&dir, BOTW);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(f.look.sensitivity, 4.0);
        assert!(w.iter().any(|w| w.contains("jump")), "{w:?}");
    }
}
