//! `game.json` and the files it points to (the run folder contract, R21 and R48).

use super::elf::Sym;
use super::layout::{PAGE, Segment};
use super::modules::Built;
use super::npdm::MainThread;
use nxb_pyjson::Value;

/// One DLC folder.
pub struct Aoc {
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
pub struct Manifest<'a> {
    /// The game's title id.
    pub title_id: u64,
    /// The game's name.
    pub name: &'a str,
    /// The version the app is built from.
    pub version: u32,
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
}

fn s(v: &str) -> Value {
    Value::Str(v.to_string())
}

fn n(v: impl TryInto<i64>) -> Value {
    Value::Int(v.try_into().unwrap_or(i64::MAX))
}

fn obj(fields: Vec<(&str, Value)>) -> Value {
    Value::Object(fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn range((offset, size): (usize, usize)) -> Value {
    obj(vec![("offset", n(offset)), ("size", n(size))])
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

fn module(b: &Built, base: u64) -> Value {
    let mut f = vec![
        ("name", s(&b.name)),
        ("base", s(&format!("{base:#x}"))),
        ("size", n(b.shape.extent().1)),
        ("text", range(b.parts[0])),
        ("rodata", range(b.parts[1])),
        ("data", range(b.parts[2])),
        ("bss_size", n(b.bss)),
        ("image", s(&format!("modules/{}.bin", b.name))),
        ("imports", s(&format!("modules/{}.imports.json", b.name))),
        ("exports", s(&format!("modules/{}.exports.json", b.name))),
    ];
    if b.name == "main" {
        f.push(("entry", n(0)));
    }
    f.push(("build_id", s(&b.build_id)));
    f.push(("segments", Value::List(b.shape.segments.iter().map(segment).collect())));
    f.push((
        "patch",
        obj(vec![("offset", n(b.shape.patch.0)), ("size", n(b.shape.patch.1))]),
    ));
    f.push((
        "key_page",
        obj(vec![("offset", n(b.shape.key_page.0)), ("size", n(b.shape.key_page.1))]),
    ));
    f.push(("fixups", s(&format!("modules/{}.fixups.tsv", b.name))));
    f.push(("gates", s(&format!("modules/{}.gates.tsv", b.name))));
    obj(f)
}

/// The text of `game.json`.
pub fn game_json(m: &Manifest) -> String {
    let mut f = vec![
        ("title_id", s(&format!("{:016X}", m.title_id))),
        ("name", s(m.name)),
        ("version", n(m.version)),
        (
            "modules",
            Value::List(m.modules.iter().map(|(b, base)| module(b, *base)).collect()),
        ),
        ("services", Value::List(m.services.iter().map(|v| s(v)).collect())),
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

/// A list of `{ "name", "slot" }`: the form of the imports and exports files.
pub fn symbols_json(v: &[Sym]) -> String {
    let list = v
        .iter()
        .map(|x| obj(vec![("name", s(&x.name)), ("slot", n(x.slot))]))
        .collect();
    let mut t = nxb_pyjson::dumps_indent(&Value::List(list), 1);
    t.push('\n');
    t
}

/// The names of the NVN list text (one a line).
pub fn nvn_list(text: &str) -> Vec<String> {
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
    fn symbols_are_a_list_of_name_and_slot() {
        let t = symbols_json(&[Sym {
            name: "nnFoo".into(),
            slot: 768,
        }]);
        let v = nxb_pyjson::parse(&t).unwrap();
        assert_eq!(nxb_pyjson::dumps(&v), r#"[{"name": "nnFoo", "slot": 768}]"#);
    }
}
