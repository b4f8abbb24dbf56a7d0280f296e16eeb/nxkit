//! The run settings of the game (`settings` of `game.json`) and the DLC folders (`aoc`).
//!
//! The build writes the answers of the title and family hooks into `settings` (R2, `docs/titles.md`). The host does
//! not use them itself and knows no game and no engine. It types the keys that name a mechanism of the core and hands
//! them to the parts that use them: the pacing, the encoder, the services and the kernel (see `seams.rs`). A key
//! that no core mechanism names, for example a setting of one engine, goes unread into `extra`, and the service that
//! the title module addressed reads it there.

use std::path::PathBuf;

use crate::error::Result;
use crate::json::Value;
use crate::manifest::{bad, field, num, text};

/// The keys that [`Settings`] types. Every other key of `settings` goes to `extra`.
const TYPED: &[&str] = &[
    "args",
    "env",
    "launch_param",
    "display_versions",
    "core_preempt",
    "present_display",
    "fps_target",
    "fps_max",
    "fps_hold",
    "present_snap",
    "pso_mtl4",
    "enc_thread",
    "screen_aspect",
    "pad_layout",
    "op_mode",
    "perf_boost",
    "tex_flush_upload",
    "tex_dirty_upload",
    "tex_dyn_slots",
    "render_cond",
    "vertex_stream_null_bind",
    "vconv_range",
    "gpu_ts_segment",
    "draw_barriers_game",
    "shader_key_skip",
    "upscaler_keys",
    "shared_font",
    "nvn_api",
    "optional_imports",
    "shared_font_decode",
    "sf_services",
];

/// The settings of the title module. The names follow `game.json`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Settings {
    /// The program arguments of the guest, one string.
    pub args: String,
    /// The environment of the guest, one string.
    pub env: String,
    /// The launch parameter the applet service gives to the game.
    pub launch_param: String,
    /// The version that the game shows for a build ID, as `(build_id, version)`.
    pub display_versions: Vec<(String, String)>,
    /// Whether the kernel preempts threads of the same core.
    pub core_preempt: bool,
    /// Whether the present path follows the display.
    pub present_display: bool,
    /// The frame rate the game aims at.
    pub fps_target: u32,
    /// The highest frame rate the game accepts.
    pub fps_max: u32,
    /// Whether the pacing holds the game at `fps_target`.
    pub fps_hold: bool,
    /// Whether a frame snaps to the display tick.
    pub present_snap: bool,
    /// Whether pipelines come from a Metal 4 archive.
    pub pso_mtl4: bool,
    /// Whether the encoder runs on its own thread.
    pub enc_thread: bool,
    /// Whether the screen keeps the aspect ratio of the game.
    pub screen_aspect: bool,
    /// The pad layout of the game, for example `nintendo`.
    pub pad_layout: String,
    /// The operation mode that the applet service reports, `handheld` or `docked`.
    pub op_mode: String,
    /// Whether the performance mode is boosted.
    pub perf_boost: bool,
    /// Whether texture uploads are flushed at the draw.
    pub tex_flush_upload: bool,
    /// Whether texture uploads only copy dirty ranges.
    pub tex_dirty_upload: bool,
    /// Whether texture slots are dynamic.
    pub tex_dyn_slots: bool,
    /// Whether conditional rendering is on.
    pub render_cond: bool,
    /// Whether the command buffer ignores a bind of one null range at a vertex stream of 16 or more.
    pub vertex_stream_null_bind: bool,
    /// Whether the vertex conversion keeps the range.
    pub vconv_range: bool,
    /// Whether GPU timestamps segment the frame.
    pub gpu_ts_segment: bool,
    /// Whether the game's draws get barriers.
    pub draw_barriers_game: bool,
    /// The part of a shader key that the pipeline lookup skips, as `(offset, length)`.
    pub shader_key_skip: (u64, u64),
    /// The keys that mark an upscaler pass.
    pub upscaler_keys: Vec<String>,
    /// The shared font to load, if the game needs one.
    pub shared_font: Option<String>,
    /// The NVN API version that the game gets, as `(major, minor)`.
    pub nvn_api: Option<(u32, u32)>,
    /// Imports that nothing provides and that stay 0, because the title says the game checks them before it calls
    /// them. Every other import that nothing provides stops the load (R15).
    pub optional_imports: Vec<String>,
    /// Whether the game decodes the shared font with the SDK's font engine and so needs the BFTTF header.
    pub shared_font_decode: bool,
    /// The services the game opens through `nn::sf::hipc` that the system service answers with zeroed output.
    pub sf_services: Vec<String>,
    /// The keys that no core mechanism names, in key order. The host does not read them. The title-addressed service
    /// reads them.
    pub extra: Vec<(String, Value)>,
}

/// One DLC folder, `aoc/<index>/` of the run folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aoc {
    /// The index the game mounts.
    pub index: u32,
    /// The title ID of the DLC package.
    pub title_id: u64,
    /// The version of the DLC.
    pub version: u32,
    /// The folder, relative to the run folder.
    pub path: PathBuf,
}

fn flag(v: &Value, key: &str) -> Result<bool> {
    field(v, key, "settings")?
        .as_bool()
        .ok_or_else(|| bad(format!("settings.{key} is not a boolean")))
}

fn small(v: &Value, key: &str) -> Result<u32> {
    u32::try_from(num(v, key, "settings")?).map_err(|_| bad(format!("settings.{key} is out of range")))
}

fn strings(v: &Value, key: &str) -> Result<Vec<String>> {
    field(v, key, "settings")?
        .as_array()
        .ok_or_else(|| bad(format!("settings.{key} is not an array")))?
        .iter()
        .map(|s| {
            s.as_str()
                .map(str::to_owned)
                .ok_or_else(|| bad(format!("settings.{key} has an entry that is not a string")))
        })
        .collect()
}

fn extra(v: &Value) -> Vec<(String, Value)> {
    match v {
        Value::Object(m) => m
            .iter()
            .filter(|(k, _)| !TYPED.contains(&k.as_str()))
            .map(|(k, x)| (k.clone(), x.clone()))
            .collect(),
        _ => Vec::new(),
    }
}

impl Settings {
    /// Reads the `settings` object. Every typed key is required except `shared_font` and `nvn_api`. The other keys go
    /// to `extra`.
    pub fn parse(v: &Value) -> Result<Settings> {
        let display_versions = field(v, "display_versions", "settings")?
            .as_array()
            .ok_or_else(|| bad("settings.display_versions is not an array"))?
            .iter()
            .map(|e| Ok((text(e, "build_id", "settings.display_versions")?, text(e, "version", "settings.display_versions")?)))
            .collect::<Result<Vec<_>>>()?;
        let skip = field(v, "shader_key_skip", "settings")?;
        let nvn_api = match v.get("nvn_api") {
            None | Some(Value::Null) => None,
            Some(a) => {
                let part = |k: &str| u32::try_from(num(a, k, "settings.nvn_api")?).map_err(|_| bad("settings.nvn_api is out of range"));
                Some((part("major")?, part("minor")?))
            }
        };
        let shared_font = match v.get("shared_font") {
            None | Some(Value::Null) => None,
            Some(_) => Some(text(v, "shared_font", "settings")?),
        };
        Ok(Settings {
            args: text(v, "args", "settings")?,
            env: text(v, "env", "settings")?,
            launch_param: text(v, "launch_param", "settings")?,
            display_versions,
            core_preempt: flag(v, "core_preempt")?,
            present_display: flag(v, "present_display")?,
            fps_target: small(v, "fps_target")?,
            fps_max: small(v, "fps_max")?,
            fps_hold: flag(v, "fps_hold")?,
            present_snap: flag(v, "present_snap")?,
            pso_mtl4: flag(v, "pso_mtl4")?,
            enc_thread: flag(v, "enc_thread")?,
            screen_aspect: flag(v, "screen_aspect")?,
            pad_layout: text(v, "pad_layout", "settings")?,
            op_mode: text(v, "op_mode", "settings")?,
            perf_boost: flag(v, "perf_boost")?,
            tex_flush_upload: flag(v, "tex_flush_upload")?,
            tex_dirty_upload: flag(v, "tex_dirty_upload")?,
            tex_dyn_slots: flag(v, "tex_dyn_slots")?,
            render_cond: flag(v, "render_cond")?,
            vertex_stream_null_bind: flag(v, "vertex_stream_null_bind")?,
            vconv_range: flag(v, "vconv_range")?,
            gpu_ts_segment: flag(v, "gpu_ts_segment")?,
            draw_barriers_game: flag(v, "draw_barriers_game")?,
            shader_key_skip: (num(skip, "offset", "settings.shader_key_skip")?, num(skip, "length", "settings.shader_key_skip")?),
            upscaler_keys: strings(v, "upscaler_keys")?,
            shared_font,
            nvn_api,
            optional_imports: match v.get("optional_imports") {
                None | Some(Value::Null) => Vec::new(),
                Some(_) => strings(v, "optional_imports")?,
            },
            shared_font_decode: v.get("shared_font_decode").and_then(Value::as_bool).unwrap_or(false),
            sf_services: match v.get("sf_services") {
                None | Some(Value::Null) => Vec::new(),
                Some(_) => strings(v, "sf_services")?,
            },
            extra: extra(v),
        })
    }
}

/// Reads the `aoc` list. A missing list means no DLC.
pub fn parse_aoc(v: Option<&Value>) -> Result<Vec<Aoc>> {
    let Some(list) = v else {
        return Ok(Vec::new());
    };
    let items = list.as_array().ok_or_else(|| bad("\"aoc\" is not an array"))?;
    items
        .iter()
        .map(|e| {
            let id = text(e, "title_id", "aoc")?;
            Ok(Aoc {
                index: u32::try_from(num(e, "index", "aoc")?).map_err(|_| bad("aoc.index is out of range"))?,
                title_id: u64::from_str_radix(id.trim_start_matches("0x"), 16)
                    .map_err(|_| bad(format!("aoc.title_id {id} is not a hex number")))?,
                version: u32::try_from(num(e, "version", "aoc")?).map_err(|_| bad("aoc.version is out of range"))?,
                path: PathBuf::from(text(e, "path", "aoc")?),
            })
        })
        .collect()
}

/// A complete `settings` object for the tests of this crate.
#[cfg(test)]
pub(crate) mod samples {
    pub(crate) const SETTINGS: &str = r#"{"args": "", "env": "", "launch_param": "", "display_versions": [{"build_id": "ab", "version": "1.6.0"}],
        "core_preempt": true, "present_display": false, "fps_target": 30, "fps_max": 60, "fps_hold": false,
        "present_snap": true, "pso_mtl4": true, "enc_thread": false, "screen_aspect": false,
        "pad_layout": "nintendo", "op_mode": "docked", "perf_boost": false, "tex_flush_upload": false, "tex_dirty_upload": false,
        "tex_dyn_slots": false, "render_cond": false, "vertex_stream_null_bind": false, "vconv_range": false, "gpu_ts_segment": false, "draw_barriers_game": false,
        "shader_key_skip": {"offset": 8, "length": 4}, "upscaler_keys": ["k"], "nvn_api": {"major": 53, "minor": 12}, "engine_key": 1}"#;
}

#[cfg(test)]
mod tests {
    use super::samples::SETTINGS;
    use super::*;
    use crate::json;

    #[test]
    fn reads_the_settings() {
        let v = json::parse(SETTINGS);
        assert!(v.is_ok());
        let Ok(v) = v else { return };
        let s = Settings::parse(&v);
        assert!(s.is_ok(), "{:?}", s.as_ref().err());
        let Ok(s) = s else { return };
        assert_eq!((s.fps_target, s.fps_max), (30, 60));
        assert!(s.present_snap && s.pso_mtl4 && s.core_preempt);
        assert_eq!(s.pad_layout, "nintendo");
        assert_eq!(s.shader_key_skip, (8, 4));
        assert_eq!(s.nvn_api, Some((53, 12)));
        assert_eq!(s.shared_font, None);
        assert_eq!(s.extra, vec![("engine_key".to_string(), Value::Int(1))]);
        assert_eq!(s.display_versions, vec![("ab".to_string(), "1.6.0".to_string())]);
    }

    #[test]
    fn reads_the_optional_imports() {
        let with = SETTINGS.replace("\"engine_key\": 1", "\"optional_imports\": [\"_ZN2nn3fs1fEv\"]");
        let v = json::parse(&with);
        assert!(v.is_ok());
        let Ok(v) = v else { return };
        let s = Settings::parse(&v);
        assert!(s.is_ok());
        let Ok(s) = s else { return };
        assert_eq!(s.optional_imports, vec!["_ZN2nn3fs1fEv".to_string()]);
        assert!(s.extra.is_empty());
    }

    #[test]
    fn refuses_a_missing_key() {
        let v = json::parse(&SETTINGS.replace("\"pso_mtl4\": true,", ""));
        assert!(v.is_ok());
        let Ok(v) = v else { return };
        assert!(Settings::parse(&v).is_err());
    }

    #[test]
    fn reads_the_aoc_list() {
        let v = json::parse(r#"[{"index": 1, "title_id": "01007EF00011F001", "version": 196608, "path": "aoc/1"}]"#);
        assert!(v.is_ok());
        let Ok(v) = v else { return };
        let a = parse_aoc(Some(&v));
        assert!(a.is_ok());
        let Ok(a) = a else { return };
        assert_eq!(a[0].index, 1);
        assert_eq!(a[0].title_id, 0x0100_7EF0_0011_F001);
        assert_eq!(a[0].path, PathBuf::from("aoc/1"));
    }
}
