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
    /// The graphics API the game renders with (`nxtitle::GraphicsApi::id`).
    pub graphics: &'static str,
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
    /// The frame limits of the family and the title (`RuntimeHooks::limits`), written as `limits`.
    pub limits: Vec<nxtitle::LimitRule>,
    /// The program arguments that set the render size (`RuntimeHooks::render_args`), written as `render_args`.
    pub render_args: Option<nxtitle::RenderArgs>,
    /// The facts of `settings.temporal_upscale`, written when the engine and the title both gave theirs.
    pub temporal_upscale: Option<(nxtitle::TemporalView, nxtitle::TemporalDraws)>,
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
        ("pace_profile", s(t.pace_profile)),
        ("screen_aspect", flag(t.screen_aspect)),
        ("pad_layout", s(t.pad_layout)),
        ("op_mode", s(t.op_mode)),
        ("spatial_audio", flag(t.spatial_audio)),
        ("perf_boost", flag(t.perf_boost)),
        ("tex_flush_upload", flag(t.tex_flush_upload)),
        ("tex_dirty_upload", flag(t.tex_dirty_upload)),
        ("tex_dyn_slots", flag(t.tex_dyn_slots)),
        ("render_cond", flag(t.render_cond)),
        ("vertex_stream_null_bind", flag(m.vertex_stream_null_bind)),
        ("vconv_range", flag(t.vconv_range)),
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
    if !t.optional_imports.is_empty() {
        f.push(("optional_imports", list(t.optional_imports)));
    }
    if let Some((major, minor)) = t.nvn_api {
        f.push((
            "nvn_api",
            obj(vec![("major", n(major)), ("minor", n(minor))]),
        ));
    }
    if let Some((view, draws)) = &m.temporal_upscale {
        f.push(("temporal_upscale", temporal_upscale(view, draws)));
    }
    obj(f)
}

fn float_pair(v: [f64; 2]) -> Value {
    Value::List(v.iter().map(|x| Value::Float(*x)).collect())
}

fn draw_slot(d: &nxtitle::DrawSlot, slot_key: &str) -> Value {
    obj(vec![("fragment", s(d.fragment)), (slot_key, n(d.slot))])
}

/// The `temporal_upscale` object of `settings`, in the shape that `nxkit-host` `tupscale.rs` reads.
fn temporal_upscale(view: &nxtitle::TemporalView, draws: &nxtitle::TemporalDraws) -> Value {
    let src = &draws.source;
    obj(vec![
        ("upscale", draw_slot(&draws.upscale, "color_slot")),
        (
            "source",
            obj(vec![
                ("fragment", s(src.fragment)),
                ("depth_slot", n(src.depth_slot)),
                ("velocity_slot", n(src.velocity_slot)),
                (
                    "view",
                    obj(vec![
                        ("stage", s(src.view_stage.id())),
                        ("slot", n(src.view_slot)),
                    ]),
                ),
            ]),
        ),
        (
            "passthrough",
            Value::List(
                draws
                    .passthrough
                    .iter()
                    .map(|d| draw_slot(d, "slot"))
                    .collect(),
            ),
        ),
        (
            "view",
            obj(vec![
                ("clip_to_prev_clip", n(view.clip_to_prev_clip)),
                ("jitter", n(view.jitter)),
                ("view_size", n(view.view_size)),
            ]),
        ),
        (
            "velocity",
            obj(vec![
                ("scale", float_pair(view.velocity.scale)),
                ("bias", float_pair(view.velocity.bias)),
                ("y_up", flag(view.velocity.y_up)),
                ("zero_means_camera", flag(view.velocity.zero_means_camera)),
            ]),
        ),
        ("depth_reversed", flag(view.depth_reversed)),
    ])
}

/// One frame limit rule of the title module, for the host's `LimitsConfig` (R30b).
fn limit(r: &nxtitle::LimitRule) -> Value {
    use nxtitle::{LimitKind as K, LimitRule, LimitSite};
    match r {
        LimitRule::Row(row) => {
            let mut kind = vec![(
                "id",
                s(match row.kind {
                    K::FpsF32 => "fps_f32",
                    K::VsyncsI32 => "vsyncs_i32",
                    K::MsF32 => "ms_f32",
                    K::FpsI32 => "fps_i32",
                    K::TicksU32 { .. } => "ticks_u32",
                    K::SecsF32 => "secs_f32",
                    K::ModeU32 => "mode_u32",
                    K::FpsF64 => "fps_f64",
                    K::IntervalMinusOneU32 => "interval_minus_one_u32",
                    K::PinU32 { .. } => "pin_u32",
                }),
            )];
            match row.kind {
                K::TicksU32 { scale } => kind.push(("scale", n(scale))),
                K::PinU32 { from, to } => {
                    kind.push(("from", n(from)));
                    kind.push(("to", n(to)));
                }
                _ => {}
            }
            let site = match row.site {
                LimitSite::Fixed { ptr_off, field } => {
                    obj(vec![("ptr_off", n(ptr_off)), ("field", n(field))])
                }
                LimitSite::Found { key, add } => obj(vec![("found", s(key)), ("add", n(add))]),
            };
            let mut f = vec![("what", s(row.what))];
            if let Some(b) = row.build {
                f.push(("build", s(b)));
            }
            f.push(("site", site));
            f.push(("kind", obj(kind)));
            f.push(("copies", n(row.copies)));
            obj(f)
        }
        LimitRule::Reflected(c) => obj(vec![(
            "reflected",
            obj(vec![
                ("class", s(c.class)),
                ("int_field", s(c.int_field)),
                ("float_field", s(c.float_field)),
                ("record_size", n(c.layout.record_size)),
                ("fields_at", n(c.layout.fields_at)),
                ("field_stride", n(c.layout.field_stride)),
                ("field_offset_at", n(c.layout.field_offset_at)),
                ("instance_type_at", n(c.layout.instance_type_at)),
            ]),
        )]),
    }
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
        ("graphics", s(m.graphics)),
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
    if let Some(r) = m.render_args {
        f.push(("render_args", obj(vec![("size", s(r.size))])));
    }
    if !m.limits.is_empty() {
        f.push(("limits", Value::List(m.limits.iter().map(limit).collect())));
    }
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

    #[test]
    fn a_limit_row_names_its_site_and_kind() {
        use nxtitle::{LimitKind, LimitRow, LimitRule, LimitSite};
        let found = limit(&LimitRule::Row(LimitRow {
            what: "Unreal t.MaxFPS",
            build: None,
            site: LimitSite::Found {
                key: "t.MaxFPS",
                add: 0x10,
            },
            kind: LimitKind::FpsF32,
            copies: 2,
        }));
        assert_eq!(
            nxb_pyjson::dumps(&found),
            r#"{"what": "Unreal t.MaxFPS", "site": {"found": "t.MaxFPS", "add": 16}, "kind": {"id": "fps_f32"}, "copies": 2}"#
        );
        let fixed = limit(&LimitRule::Row(LimitRow {
            what: "w",
            build: Some("ab"),
            site: LimitSite::Fixed {
                ptr_off: 8,
                field: 4,
            },
            kind: LimitKind::TicksU32 { scale: 19_200_000 },
            copies: 1,
        }));
        assert_eq!(
            nxb_pyjson::dumps(&fixed),
            r#"{"what": "w", "build": "ab", "site": {"ptr_off": 8, "field": 4}, "kind": {"id": "ticks_u32", "scale": 19200000}, "copies": 1}"#
        );
    }

    /// The temporal upscale object of OCTOPATH TRAVELER, read back with the host's expectations (`tupscale.rs`).
    #[test]
    fn temporal_upscale_has_the_shape_the_host_reads() {
        let c = nxtitles::catalog().unwrap();
        let r = c.resolve(&nxtitle::TitleFacts::new(nxtitle::TitleId(
            0x0100_57D0_0649_2000,
        )));
        let (view, draws) = (
            r.temporal_view().unwrap().value,
            r.temporal_draws().unwrap().value,
        );
        let text = nxb_pyjson::dumps_indent(&temporal_upscale(&view, &draws), 1);
        println!("{text}");
        let v = nxb_pyjson::parse(&text).unwrap();
        let int = |o: &Value, k: &str| o.get(k).and_then(Value::as_i64).unwrap();
        let pair = |o: &Value, k: &str| -> Vec<f64> {
            o.get(k)
                .and_then(Value::as_list)
                .unwrap()
                .iter()
                .map(|x| match x {
                    Value::Float(f) => *f,
                    other => panic!("{other:?}"),
                })
                .collect()
        };
        let up = v.get("upscale").unwrap();
        assert_eq!(
            up.get("fragment").and_then(Value::as_str),
            Some("be89b1ada1308881")
        );
        assert_eq!(int(up, "color_slot"), 0);
        let src = v.get("source").unwrap();
        assert_eq!((int(src, "depth_slot"), int(src, "velocity_slot")), (3, 0));
        let ubo = src.get("view").unwrap();
        assert_eq!(ubo.get("stage").and_then(Value::as_str), Some("fragment"));
        assert_eq!(int(ubo, "slot"), 0);
        let pass = v.get("passthrough").and_then(Value::as_list).unwrap();
        assert_eq!(pass.len(), 2);
        assert_eq!(int(&pass[1], "slot"), 2);
        let offsets = v.get("view").unwrap();
        assert_eq!(
            [
                int(offsets, "clip_to_prev_clip"),
                int(offsets, "jitter"),
                int(offsets, "view_size")
            ],
            [1760, 1824, 1888]
        );
        let vel = v.get("velocity").unwrap();
        assert_eq!(pair(vel, "scale"), [4.008016, 4.008016]);
        assert_eq!(pair(vel, "bias"), [-2.003976, -2.003976]);
        assert_eq!(vel.get("y_up"), Some(&Value::Bool(true)));
        assert_eq!(vel.get("zero_means_camera"), Some(&Value::Bool(true)));
        assert_eq!(v.get("depth_reversed"), Some(&Value::Bool(true)));
    }
}
