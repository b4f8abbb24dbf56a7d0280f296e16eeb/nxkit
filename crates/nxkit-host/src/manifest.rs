//! `game.json`, the manifest of the run folder (see `docs/nxkit-host.md`), and the list files it names.

use std::path::{Path, PathBuf};

use crate::error::{HostError, Result};
use crate::json::{self, Value};
use crate::module::{self, Module};
use crate::settings::{self, Aoc, Settings};

/// The page size that the build wrote the segments for.
const PAGE: u64 = 0x4000;

/// The main thread of the game, from its NPDM.
#[derive(Debug, Clone, Copy)]
pub struct MainThread {
    /// The Switch priority.
    pub priority: i32,
    /// The core, or -2 for any.
    pub core: i32,
    /// The stack size in bytes.
    pub stack_size: u64,
}

/// The memory profile of the game: the sizes of the regions that the host reserves beyond the modules, and the
/// numbers the kernel reports to the game with `GetInfo`. The build writes them from the title module. The host has
/// no defaults (R2).
#[derive(Debug, Clone, Copy)]
pub struct Memory {
    /// The heap region of `SetHeapSize`, in bytes.
    pub heap: u64,
    /// The alias region of `MapMemory`, in bytes.
    pub alias: u64,
    /// The stack region of the process, in bytes. `MapMemory` takes its destination from it.
    pub stack: u64,
    /// The application memory pool in bytes. `SetHeapSize` and `MapPhysicalMemory` fail beyond it.
    pub app_pool: u64,
    /// The used memory that `GetInfo` reports beside the heap, in bytes.
    pub mem_used: u64,
    /// The system resource size of the NPDM. 0 means that the game has no virtual address memory.
    pub sys_resource: u64,
    /// Whether `MapPhysicalMemory` fails when it would take the process past the application pool.
    pub map_limit: bool,
    /// The largest block that the standard allocator of the guest may hand out, in bytes. 0 means no cap.
    pub stdalloc_cap: u64,
    /// Whether the allocator of the guest aligns big blocks to 2 MiB.
    pub alloc_big_align: bool,
}

/// The space that `reserve` keeps below the heap for the Horizon layout of a process, 256 MiB. `reserve` is the whole
/// range that the game gets, so the heap is what is left after the front space, the stack region and the alias region.
const RESERVE_FRONT: u64 = 0x1000_0000;

/// `game.json`.
#[derive(Debug, Clone)]
pub struct Game {
    /// The folder of `game.json`. Every relative path starts here.
    pub root: PathBuf,
    /// The title ID as a hex string.
    pub title_id: String,
    /// The name of the game.
    pub name: String,
    /// The version as the game shows it, for example `1.6.0`.
    pub version: String,
    /// The version as the number of the update.
    pub version_number: u32,
    /// The modules, in load order.
    pub modules: Vec<Module>,
    /// The names of the enabled services, as `nxtitle::Service::id` values.
    pub services: Vec<String>,
    /// The romfs folder.
    pub romfs: PathBuf,
    /// The patch data of the title module, if the build wrote any.
    pub patches: Option<PathBuf>,
    /// The main thread.
    pub main_thread: MainThread,
    /// The region sizes. The defaults apply to a key that is absent.
    pub memory: Memory,
    /// The raw `main.npdm`.
    pub npdm: Option<PathBuf>,
    /// The library with the module segments, relative to the run folder.
    pub dylib: PathBuf,
    /// The run settings of the title module.
    pub settings: Settings,
    /// The DLC folders.
    pub aoc: Vec<Aoc>,
}

/// One entry of an export list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    /// The Switch mangled name.
    pub name: String,
    /// The offset from the module base of the symbol.
    pub slot: u64,
}

/// One entry of an import list: one slot of the module that an undefined symbol fills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// The Switch mangled name.
    pub name: String,
    /// The offset from the module base of the slot to fill.
    pub slot: u64,
    /// What the relocation adds to the address. It is 0 for a `JUMP_SLOT`.
    pub addend: i64,
    /// Whether the symbol is weak. A weak symbol that nothing provides is stored as 0.
    pub weak: bool,
}

pub(crate) fn bad(m: impl Into<String>) -> HostError {
    HostError::Manifest(m.into())
}

pub(crate) fn field<'a>(v: &'a Value, key: &str, ctx: &str) -> Result<&'a Value> {
    v.get(key).ok_or_else(|| bad(format!("{ctx}: missing \"{key}\"")))
}

pub(crate) fn num(v: &Value, key: &str, ctx: &str) -> Result<u64> {
    field(v, key, ctx)?
        .as_u64()
        .ok_or_else(|| bad(format!("{ctx}: \"{key}\" is not a number")))
}

pub(crate) fn text(v: &Value, key: &str, ctx: &str) -> Result<String> {
    field(v, key, ctx)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| bad(format!("{ctx}: \"{key}\" is not a string")))
}

/// Reads `memory`. Every key is required: the host holds no numbers of any game (R2), so a missing key is an error.
fn memory(v: &Value) -> Result<Memory> {
    let block = field(v, "memory", "game.json")?;
    let n = |key: &str| num(block, key, "memory");
    let flag = |key: &str| -> Result<bool> {
        field(block, key, "memory")?
            .as_bool()
            .ok_or_else(|| bad(format!("memory.{key} is not a boolean")))
    };
    let (reserve, alias, stack) = (n("reserve")?, n("alias")?, n("stack")?);
    // The heap is the part of `reserve` that the front space, the stack region and the alias region leave.
    let heap = reserve
        .checked_sub(alias)
        .and_then(|r| r.checked_sub(stack))
        .and_then(|r| r.checked_sub(RESERVE_FRONT))
        .ok_or_else(|| bad("memory.reserve is smaller than alias, stack and 0x10000000"))?;
    Ok(Memory {
        heap,
        alias,
        stack,
        app_pool: n("app_pool")?,
        mem_used: n("mem_used")?,
        sys_resource: n("sys_resource")?,
        map_limit: flag("map_limit")?,
        stdalloc_cap: n("stdalloc_cap")?,
        alloc_big_align: flag("alloc_big_align")?,
    })
}

fn main_thread(v: &Value) -> Result<MainThread> {
    let int = |key: &str| -> Result<i32> {
        let n = field(v, key, "main_thread")?
            .as_i64()
            .ok_or_else(|| bad(format!("main_thread.{key} is not a number")))?;
        i32::try_from(n).map_err(|_| bad(format!("main_thread.{key} is out of range")))
    };
    Ok(MainThread {
        priority: int("priority")?,
        core: int("core")?,
        stack_size: num(v, "stack_size", "main_thread")?,
    })
}

impl Game {
    /// Reads `<root>/game.json`.
    pub fn read(root: &Path) -> Result<Game> {
        let path = root.join("game.json");
        let src = std::fs::read_to_string(&path).map_err(|e| HostError::io(&path, &e))?;
        Game::parse(root, &src)
    }

    /// Parses the text of a `game.json`. Keys that the host does not use are ignored: `nvn_functions` (the GPU
    /// service has its own table of NVN names) and, per module, `fixups` is kept but never read.
    pub fn parse(root: &Path, src: &str) -> Result<Game> {
        let v = json::parse(src)?;
        let ctx = "game.json";
        let modules = field(&v, "modules", ctx)?
            .as_array()
            .ok_or_else(|| bad("\"modules\" is not an array"))?
            .iter()
            .enumerate()
            .map(|(i, m)| module::parse(m, i))
            .collect::<Result<Vec<_>>>()?;
        let services = field(&v, "services", ctx)?
            .as_array()
            .ok_or_else(|| bad("\"services\" is not an array"))?
            .iter()
            .map(|s| {
                s.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| bad("a service name is not a string"))
            })
            .collect::<Result<Vec<_>>>()?;
        let opt_path = |key: &str| v.get(key).and_then(Value::as_str).map(PathBuf::from);
        let game = Game {
            root: root.to_path_buf(),
            title_id: text(&v, "title_id", ctx)?,
            name: text(&v, "name", ctx)?,
            version: text(&v, "version", ctx)?,
            version_number: u32::try_from(num(&v, "version_number", ctx)?)
                .map_err(|_| bad("\"version_number\" is out of range"))?,
            modules,
            services,
            romfs: PathBuf::from(text(&v, "romfs", ctx)?),
            patches: opt_path("patches"),
            main_thread: main_thread(field(&v, "main_thread", ctx)?)?,
            memory: memory(&v)?,
            npdm: opt_path("npdm"),
            dylib: PathBuf::from(text(&v, "dylib", ctx)?),
            settings: Settings::parse(field(&v, "settings", ctx)?)?,
            aoc: settings::parse_aoc(v.get("aoc"))?,
        };
        let page = num(&v, "page_size", ctx)?;
        if page != PAGE {
            return Err(bad(format!("\"page_size\" is {page:#x}, this host runs with {PAGE:#x}")));
        }
        game.check()?;
        Ok(game)
    }

    /// The module that has the entry point.
    pub fn main_module(&self) -> Result<&Module> {
        self.modules
            .iter()
            .find(|m| m.entry.is_some())
            .ok_or_else(|| bad("no module has an \"entry\""))
    }

    /// The game with every module moved by `delta`. dyld slides the library, so the modules sit at the preferred
    /// address plus the slide.
    pub fn moved(&self, delta: i64) -> Result<Game> {
        let modules = self
            .modules
            .iter()
            .map(|m| {
                m.moved(delta)
                    .ok_or_else(|| bad(format!("module {} leaves the address space when moved by {delta:#x}", m.name)))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Game {
            modules,
            ..self.clone()
        })
    }

    /// Checks what the loader relies on: the segments of each module, no overlap between modules and one entry.
    fn check(&self) -> Result<()> {
        for m in &self.modules {
            m.check()?;
        }
        if self.modules.is_empty() {
            return Err(bad("\"modules\" is empty"));
        }
        let mut spans: Vec<(u64, u64, &str)> = self.modules.iter().map(|m| (m.start(), m.end(), m.name.as_str())).collect();
        spans.sort_unstable();
        for w in spans.windows(2) {
            if w[0].1 > w[1].0 {
                return Err(bad(format!("modules {} and {} overlap", w[0].2, w[1].2)));
            }
        }
        if self.modules.iter().filter(|m| m.entry.is_some()).count() != 1 {
            return Err(bad("exactly one module must have an \"entry\""));
        }
        Ok(())
    }

    /// The path of a file named in `game.json`.
    pub fn path(&self, rel: &Path) -> PathBuf {
        self.root.join(rel)
    }

    /// The DLC folders as `(index, absolute path)`, in the order of `game.json`.
    pub fn aoc_dirs(&self) -> Vec<(u32, PathBuf)> {
        self.aoc.iter().map(|a| (a.index, self.path(&a.path))).collect()
    }
}

/// Reads an export list: a JSON array of `{ "name", "slot" }`.
pub fn read_slots(path: &Path) -> Result<Vec<Slot>> {
    let src = std::fs::read_to_string(path).map_err(|e| HostError::io(path, &e))?;
    parse_slots(&src).map_err(|e| bad(format!("{}: {e}", path.display())))
}

/// Reads an import list: a JSON array of `{ "name", "slot", "addend", "weak" }`.
pub fn read_imports(path: &Path) -> Result<Vec<Import>> {
    let src = std::fs::read_to_string(path).map_err(|e| HostError::io(path, &e))?;
    parse_imports(&src).map_err(|e| bad(format!("{}: {e}", path.display())))
}

/// Parses the text of a list file.
pub fn parse_slots(src: &str) -> Result<Vec<Slot>> {
    let v = json::parse(src)?;
    let a = v.as_array().ok_or_else(|| bad("the list is not an array"))?;
    a.iter()
        .map(|e| {
            Ok(Slot {
                name: text(e, "name", "list entry")?,
                slot: num(e, "slot", "list entry")?,
            })
        })
        .collect()
}

/// Parses the text of an import list file.
pub fn parse_imports(src: &str) -> Result<Vec<Import>> {
    let v = json::parse(src)?;
    let a = v.as_array().ok_or_else(|| bad("the list is not an array"))?;
    a.iter()
        .map(|e| {
            Ok(Import {
                name: text(e, "name", "import entry")?,
                slot: num(e, "slot", "import entry")?,
                addend: field(e, "addend", "import entry")?
                    .as_i64()
                    .ok_or_else(|| bad("import entry: \"addend\" is not a number"))?,
                weak: field(e, "weak", "import entry")?
                    .as_bool()
                    .ok_or_else(|| bad("import entry: \"weak\" is not a boolean"))?,
            })
        })
        .collect()
}

/// A complete `game.json` for the tests of this crate. `modules` is the text of the array.
#[cfg(test)]
pub(crate) fn sample(extra: &str, modules: &str) -> String {
    format!(
        r#"{{"title_id": "01007EF00011E000", "name": "Zelda", "version": "1.6.0", "version_number": 1114112,
        "services": ["kernel", "offline"], "romfs": "romfs", "dylib": "nxgame.dylib", "page_size": 16384,
        "main_thread": {{"priority": 44, "core": 0, "stack_size": "0x100000"}},
        "memory": {{"reserve": 4294967296, "alias": 268435456, "stack": 268435456, "app_pool": 3444572160,
          "mem_used": 67108864, "sys_resource": 33554432, "map_limit": true, "stdalloc_cap": 5, "alloc_big_align": true}},
        "settings": {}, "nvn_functions": ["nvnDeviceInitialize"], {extra}
        "modules": [{modules}]}}"#,
        crate::settings::samples::SETTINGS
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::samples::NEAR;

    fn parse(extra: &str) -> Result<Game> {
        Game::parse(Path::new("/x"), &sample(extra, NEAR))
    }

    #[test]
    fn parses_a_sample() {
        let r = parse("");
        assert!(r.is_ok(), "{:?}", r.as_ref().err());
        let Ok(g) = r else { return };
        assert_eq!(g.modules[0].base, 0x1_0000_0000);
        assert_eq!(g.modules[0].end(), 0x1_0002_0000);
        assert_eq!(g.main_thread.stack_size, 0x10_0000);
        assert_eq!((g.version.as_str(), g.version_number), ("1.6.0", 1_114_112));
        assert_eq!(g.settings.fps_target, 30);
        assert_eq!(g.main_module().map(|m| m.name.as_str()).ok(), Some("main"));
    }

    #[test]
    fn the_heap_follows_from_reserve() {
        let r = parse("");
        assert!(r.is_ok(), "{:?}", r.as_ref().err());
        let Ok(g) = r else { return };
        assert_eq!(g.memory.heap, 0xd000_0000);
        assert_eq!(g.memory.app_pool, 0xcd50_0000);
        assert!(g.memory.map_limit && g.memory.alloc_big_align);
        assert_eq!(g.memory.stdalloc_cap, 5);
        assert_eq!(g.memory.mem_used, 0x400_0000);
    }

    #[test]
    fn refuses_a_reserve_that_is_too_small() {
        let s = sample("", NEAR).replace("\"reserve\": 4294967296", "\"reserve\": \"0x1000\"");
        assert!(Game::parse(Path::new("/x"), &s).is_err());
    }

    #[test]
    fn refuses_a_missing_memory_key() {
        let s = sample("", NEAR).replace("\"app_pool\": 3444572160,", "");
        assert!(Game::parse(Path::new("/x"), &s).is_err());
    }

    #[test]
    fn refuses_another_page_size() {
        let s = sample("", NEAR).replace("\"page_size\": 16384", "\"page_size\": 4096");
        assert!(Game::parse(Path::new("/x"), &s).is_err());
    }

    #[test]
    fn refuses_a_missing_segment_list() {
        let s = sample("", &NEAR.replace("\"segments\"", "\"parts\""));
        assert!(Game::parse(Path::new("/x"), &s).is_err());
    }

    #[test]
    fn moves_every_module() {
        let g = parse("").and_then(|g| g.moved(0x8000));
        assert!(g.is_ok());
        let Ok(g) = g else { return };
        assert_eq!(g.modules[0].base, 0x1_0000_8000);
    }

    #[test]
    fn reads_the_aoc_folders() {
        let r = parse("\"aoc\": [{\"index\": 1, \"title_id\": \"01007EF00011F001\", \"version\": 196608, \"path\": \"aoc/1\"}],");
        assert!(r.is_ok(), "{:?}", r.as_ref().err());
        let Ok(g) = r else { return };
        assert_eq!(g.aoc_dirs(), vec![(1, PathBuf::from("/x/aoc/1"))]);
    }

    #[test]
    fn reads_a_list() {
        let l = parse_slots(r#"[{"name": "_ZN2nn2os5SleepE", "slot": "0x40"}]"#).unwrap_or_default();
        assert_eq!(l, vec![Slot { name: "_ZN2nn2os5SleepE".into(), slot: 0x40 }]);
    }

    #[test]
    fn reads_an_import_list_with_addend_and_weak() {
        let l = parse_imports(r#"[{"name": "f", "slot": "0x40", "addend": -8, "weak": true}]"#).unwrap_or_default();
        assert_eq!(l, vec![Import { name: "f".into(), slot: 0x40, addend: -8, weak: true }]);
        assert!(parse_imports(r#"[{"name": "f", "slot": 64}]"#).is_err());
    }
}
