//! `game.json` and the files it points to (the run folder contract, R21 and R48).

use super::elf::{Import, Sym};
use super::layout::{PAGE, Segment};
use super::modules::Built;
use super::npdm::MainThread;
use nxb_pyjson::Value;

/// One DLC folder.
pub(crate) struct Aoc {
    /// The index the game mounts (`dlc title id - app id - 0x1000`).
    pub index: u32,
    /// The DLC's title id.
    pub title_id: u64,
    /// The DLC's version.
    pub version: u32,
    /// The folder, relative to the run folder.
    pub path: String,
}

/// What `game.json` is made from.
pub(crate) struct Manifest<'a> {
    /// The game's title id.
    pub title_id: u64,
    /// The game's name.
    pub name: &'a str,
    /// The version the app is built from, as the number of the update.
    pub version: u32,
    /// The version as the game shows it (`1.6.0`).
    pub version_name: String,
    /// The rewritten modules in load order, with their bases.
    pub modules: &'a [(Built, u64)],
    /// The `Service::id` of every service nxkit runs.
    pub services: Vec<&'static str>,
    /// The names the NVN FFI answers.
    pub nvn_functions: Vec<String>,
    /// Whether the title has patch rules (`patches.toml`).
    pub patches: bool,
    /// From the NPDM.
    pub main_thread: MainThread,
    /// The DLC folders.
    pub aoc: &'a [Aoc],
    /// The run settings of the title module.
    pub settings: nxtitle::Settings,
    /// Whether the game decodes the shared font itself (`RuntimeHooks::shared_font_decode`).
    pub shared_font_decode: bool,
    /// The services the game opens through `nn::sf::hipc` (`RuntimeHooks::sf_services`).
    pub sf_services: Vec<&'static str>,
    /// The need `vertex_stream_null_bind` of the title.
    pub vertex_stream_null_bind: bool,
}

fn s(v: &str) -> Value {
    Value::Str(v.to_string())
}

fn n(v: impl TryInto<i64>) -> Value {
    Value::Int(v.try_into().unwrap_or(i64::MAX))
}

fn obj(fields: Vec<(&str, Value)>) -> Value {
    Value::Object(
        fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    )
}

fn range((offset, size): (usize, usize)) -> Value {
    obj(vec![("offset", n(offset)), ("size", n(size))])
}

fn flag(v: bool) -> Value {
    Value::Bool(v)
}

fn list(v: &[&str]) -> Value {
    Value::List(v.iter().map(|x| s(x)).collect())
}

/// The `memory` object: sizes in bytes, as numbers.
fn memory(m: &nxtitle::Memory) -> Value {
    obj(vec![
        ("reserve", n(m.reserve)),
        ("alias", n(m.alias)),
        ("stack", n(m.stack)),
        ("app_pool", n(m.app_pool)),
        ("mem_used", n(m.used)),
        ("sys_resource", n(m.system_resource)),
        ("map_limit", flag(m.map_limit)),
        ("stdalloc_cap", n(m.stdalloc_cap)),
        ("alloc_big_align", flag(m.alloc_big_align)),
    ])
}

/// The `settings` object: the other keys of the engine's `game_profile.h`.
fn settings(m: &Manifest) -> Value {
    let t = &m.settings;
    let mut f = vec![
        ("args", s(t.args)),
        ("env", s(t.env)),
        ("launch_param", s(t.launch_param)),
        (
            "display_versions",
            Value::List(
                t.display_versions
                    .iter()
                    .map(|(id, v)| obj(vec![("build_id", s(id)), ("version", s(v))]))
                    .collect(),
            ),
        ),
        ("core_preempt", flag(t.core_preempt)),
        ("present_display", flag(t.present_display)),
        ("fps_target", n(t.fps_target)),
        ("fps_max", n(t.fps_max)),
        ("fps_hold", flag(t.fps_hold)),
        ("texture_pool_mb", n(t.texture_pool_mb)),
        ("present_snap", flag(t.present_snap)),
        ("pso_mtl4", flag(t.pso_mtl4)),
        ("enc_thread", flag(t.enc_thread)),
        ("overlap", flag(t.overlap)),
        ("screen_aspect", flag(t.screen_aspect)),
        ("pad_layout", s(t.pad_layout)),
        ("op_mode", s(t.op_mode)),
        ("perf_boost", flag(t.perf_boost)),
        ("tex_flush_upload", flag(t.tex_flush_upload)),
        ("tex_dirty_upload", flag(t.tex_dirty_upload)),
        ("tex_dyn_slots", flag(t.tex_dyn_slots)),
        ("render_cond", flag(t.render_cond)),
        ("vertex_stream_null_bind", flag(m.vertex_stream_null_bind)),
        ("vconv_range", flag(t.vconv_range)),
        ("gpu_ts_segment", flag(t.gpu_ts_segment)),
        ("draw_barriers_game", flag(t.draw_barriers_game)),
        (
            "shader_key_skip",
            obj(vec![
                ("offset", n(t.shader_key_skip.0)),
                ("length", n(t.shader_key_skip.1)),
            ]),
        ),
        ("upscaler_keys", list(t.upscaler_keys)),
        ("shared_font_decode", flag(m.shared_font_decode)),
        ("sf_services", list(&m.sf_services)),
    ];
    if let Some(cvar) = t.fps_cvar {
        f.push(("fps_cvar", s(cvar)));
    }
    if let Some(font) = t.shared_font {
        f.push(("shared_font", s(font)));
    }
    if let Some((major, minor)) = t.nvn_api {
        f.push((
            "nvn_api",
            obj(vec![("major", n(major)), ("minor", n(minor))]),
        ));
    }
    obj(f)
}

fn segment(g: &Segment) -> Value {
    obj(vec![
        ("name", s(&g.name)),
        ("offset", n(g.offset)),
        ("size", n(g.mem_size)),
        ("file_offset", n(g.file_off)),
        ("file_size", n(g.file_len)),
        ("prot", s(if g.exec { "r-x" } else { "rw-" })),
    ])
}

/// The data area, widened down to the start of its segment: the host wants the offset on a 16K page, and the data
/// of an NSO starts on a 4K page.
fn data_range(b: &Built) -> Value {
    let (off, size) = b.parts[2];
    let start = b
        .shape
        .segments
        .iter()
        .find(|g| g.kind == "DATA")
        .map_or(off, |g| usize::try_from(g.offset).unwrap_or(off));
    range((start, (off + size).saturating_sub(start)))
}

fn module(b: &Built, base: u64) -> Value {
    let (low, high) = b.shape.extent();
    let mut f = vec![
        ("name", s(&b.name)),
        ("base", s(&format!("{base:#x}"))),
        ("size", n(high)),
        ("low", n(low)),
        ("span", n(high.wrapping_add_signed(low.wrapping_neg()))),
        ("text", range(b.parts[0])),
        ("rodata", range(b.parts[1])),
        ("data", data_range(b)),
        ("bss_size", n(b.bss)),
        ("image", s(&format!("modules/{}.bin", b.name))),
        ("imports", s(&format!("modules/{}.imports.json", b.name))),
        ("exports", s(&format!("modules/{}.exports.json", b.name))),
    ];
    if b.name == "main" {
        f.push(("entry", n(0)));
    }
    f.push(("build_id", s(&b.build_id)));
    f.push((
        "segments",
        Value::List(b.shape.segments.iter().map(segment).collect()),
    ));
    f.push((
        "patch",
        obj(vec![
            ("offset", n(b.shape.patch.0)),
            ("size", n(b.shape.patch.1)),
        ]),
    ));
    f.push(("key_page", n(b.shape.key_page.0)));
    f.push(("fixups", s(&format!("modules/{}.fixups.tsv", b.name))));
    f.push(("gates", s(&format!("modules/{}.gates.tsv", b.name))));
    obj(f)
}

/// The text of `game.json`.
pub(crate) fn game_json(m: &Manifest) -> String {
    let mut f = vec![
        ("title_id", s(&format!("{:016X}", m.title_id))),
        ("name", s(m.name)),
        ("version", s(&m.version_name)),
        ("version_number", n(m.version)),
        (
            "modules",
            Value::List(m.modules.iter().map(|(b, base)| module(b, *base)).collect()),
        ),
        (
            "services",
            Value::List(m.services.iter().map(|v| s(v)).collect()),
        ),
        (
            "nvn_functions",
            Value::List(m.nvn_functions.iter().map(|v| s(v)).collect()),
        ),
        ("romfs", s("romfs")),
    ];
    if m.patches {
        f.push(("patches", s("patches.toml")));
    }
    f.push((
        "main_thread",
        obj(vec![
            ("priority", n(m.main_thread.priority)),
            ("core", n(m.main_thread.core)),
            ("stack_size", n(m.main_thread.stack_size)),
        ]),
    ));
    f.push(("npdm", s("main.npdm")));
    f.push(("memory", memory(&m.settings.memory)));
    f.push(("settings", settings(m)));
    f.push(("dylib", s("nxgame.dylib")));
    f.push(("page_size", n(PAGE)));
    f.push((
        "aoc",
        Value::List(
            m.aoc
                .iter()
                .map(|a| {
                    obj(vec![
                        ("index", n(a.index)),
                        ("title_id", s(&format!("{:016X}", a.title_id))),
                        ("version", n(a.version)),
                        ("path", s(&a.path)),
                    ])
                })
                .collect(),
        ),
    ));
    let mut t = nxb_pyjson::dumps_indent(&obj(f), 1);
    t.push('\n');
    t
}

fn list_json(list: Vec<Value>) -> String {
    let mut t = nxb_pyjson::dumps_indent(&Value::List(list), 1);
    t.push('\n');
    t
}

/// A list of `{ "name", "slot" }`: the form of the exports file.
pub(crate) fn symbols_json(v: &[Sym]) -> String {
    list_json(
        v.iter()
            .map(|x| obj(vec![("name", s(&x.name)), ("slot", n(x.slot))]))
            .collect(),
    )
}

/// A list of `{ "name", "slot", "addend", "weak" }`: the form of the imports file. One entry for each slot.
pub(crate) fn imports_json(v: &[Import]) -> String {
    list_json(
        v.iter()
            .map(|x| {
                obj(vec![
                    ("name", s(&x.name)),
                    ("slot", n(x.slot)),
                    ("addend", n(x.addend)),
                    ("weak", flag(x.weak)),
                ])
            })
            .collect(),
    )
}

/// The names of the NVN list text (one a line).
pub(crate) fn nvn_list(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nvn_list_skips_blank_and_comment_lines() {
        assert_eq!(nvn_list("# x\nnvnA\n\n nvnB \n"), ["nvnA", "nvnB"]);
    }

    #[test]
    fn the_memory_object_carries_the_layout() {
        let mut t = nxtitle::Settings::DEFAULT;
        t.memory.app_pool = 0xCD50_0000;
        let v = memory(&t.memory);
        let text = nxb_pyjson::dumps(&v);
        assert!(text.contains(r#""app_pool": 3444572160"#), "{text}");
        assert!(text.contains(r#""map_limit": false"#));
        assert!(
            text.contains(r#""mem_used": "#) && text.contains(r#""sys_resource": "#),
            "{text}"
        );
    }

    #[test]
    fn symbols_are_a_list_of_name_and_slot() {
        let t = symbols_json(&[Sym {
            name: "nnFoo".into(),
            slot: 768,
        }]);
        let v = nxb_pyjson::parse(&t).unwrap();
        assert_eq!(nxb_pyjson::dumps(&v), r#"[{"name": "nnFoo", "slot": 768}]"#);
    }

    #[test]
    fn imports_carry_the_addend_and_the_weak_flag() {
        let t = imports_json(&[Import {
            name: "nnFoo".into(),
            slot: 768,
            addend: 8,
            weak: true,
        }]);
        let v = nxb_pyjson::parse(&t).unwrap();
        assert_eq!(
            nxb_pyjson::dumps(&v),
            r#"[{"name": "nnFoo", "slot": 768, "addend": 8, "weak": true}]"#
        );
    }
}
