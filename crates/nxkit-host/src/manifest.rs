//! `game.json`, the manifest of the run folder (see `docs/nxkit-host.md`), and the list files it names.

use std::path::{Path, PathBuf};

use crate::error::{HostError, Result};
use crate::json::{self, Value};

/// An area of a module, as an offset from the module base and a size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Region {
    /// Offset from the module base.
    pub offset: u64,
    /// Size in bytes.
    pub size: u64,
}

/// One module of the game.
#[derive(Debug, Clone)]
pub struct Module {
    /// `rtld`, `main`, `subsdk0` and so on, and `sdk`.
    pub name: String,
    /// The fixed guest address.
    pub base: u64,
    /// The size of the module from `base`, in bytes. It includes the key page and the bss.
    pub size: u64,
    /// The code.
    pub text: Region,
    /// The read-only data.
    pub rodata: Region,
    /// The data. The bss follows it.
    pub data: Region,
    /// The size of the bss, which starts at the end of `data`.
    pub bss_size: u64,
    /// The rewritten flat image, relative to the run folder. The copy fallback reads it.
    pub image: PathBuf,
    /// The import list, relative to the run folder.
    pub imports: PathBuf,
    /// The export list, relative to the run folder.
    pub exports: PathBuf,
    /// The offset of the entry point. Only `main` has one.
    pub entry: Option<u64>,
    /// The offset of the key page, which is 0x4000 bytes (`KEY_PAGE_OFF` of the rewriter).
    pub key_page: u64,
    /// The gate list of the rewriter (`<name>.gates.tsv`), relative to the run folder.
    pub gates: PathBuf,
}

impl Module {
    /// The first byte after the module. `size` includes the bss.
    pub fn end(&self) -> u64 {
        self.base.saturating_add(self.size)
    }

    /// The offset where the bss starts, right after the data.
    pub fn bss_offset(&self) -> u64 {
        self.data.offset.saturating_add(self.data.size)
    }
}

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
/// numbers the kernel reports to the game with `GetInfo`. The keys have the meaning of the nxnative game profile
/// (`mem_reserve`, `mem_alias`, `mem_stack`, `app_pool`, `mem_used`, `sys_resource`, `mem_map_limit`).
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
}

/// The space in front of the heap in `reserve`, as in nxnative: 256 MiB for what the Horizon layout keeps below the heap.
const RESERVE_FRONT: u64 = 0x1000_0000;

impl Default for Memory {
    fn default() -> Self {
        Memory {
            heap: 0x1_4000_0000,
            alias: 0x8000_0000,
            stack: 0x1000_0000,
            app_pool: 0xcd50_0000,
            mem_used: 0x400_0000,
            sys_resource: 0x200_0000,
            map_limit: false,
        }
    }
}

/// `game.json`.
#[derive(Debug, Clone)]
pub struct Game {
    /// The folder of `game.json`. Every relative path starts here.
    pub root: PathBuf,
    /// The title ID as a hex string.
    pub title_id: String,
    /// The game version.
    pub version: String,
    /// The modules, in load order.
    pub modules: Vec<Module>,
    /// The names of the enabled services, as `nxtitle::Service` names.
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
}

/// One entry of an import or export list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    /// The Switch mangled name.
    pub name: String,
    /// The offset from the module base. For an import it is the slot to fill. For an export it is the symbol.
    pub slot: u64,
}

fn bad(m: impl Into<String>) -> HostError {
    HostError::Manifest(m.into())
}

fn field<'a>(v: &'a Value, key: &str, ctx: &str) -> Result<&'a Value> {
    v.get(key).ok_or_else(|| bad(format!("{ctx}: missing \"{key}\"")))
}

fn num(v: &Value, key: &str, ctx: &str) -> Result<u64> {
    field(v, key, ctx)?
        .as_u64()
        .ok_or_else(|| bad(format!("{ctx}: \"{key}\" is not a number")))
}

fn text(v: &Value, key: &str, ctx: &str) -> Result<String> {
    field(v, key, ctx)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| bad(format!("{ctx}: \"{key}\" is not a string")))
}

fn region(v: &Value, key: &str, ctx: &str) -> Result<Region> {
    let r = field(v, key, ctx)?;
    let c = format!("{ctx}.{key}");
    Ok(Region {
        offset: num(r, "offset", &c)?,
        size: num(r, "size", &c)?,
    })
}

fn module(v: &Value, i: usize) -> Result<Module> {
    let name = text(v, "name", &format!("modules[{i}]"))?;
    let ctx = format!("module {name}");
    let entry = match v.get("entry") {
        None | Some(Value::Null) => None,
        Some(e) => Some(e.as_u64().ok_or_else(|| bad(format!("{ctx}: \"entry\" is not a number")))?),
    };
    Ok(Module {
        base: num(v, "base", &ctx)?,
        size: num(v, "size", &ctx)?,
        text: region(v, "text", &ctx)?,
        rodata: region(v, "rodata", &ctx)?,
        data: region(v, "data", &ctx)?,
        bss_size: num(v, "bss_size", &ctx)?,
        image: PathBuf::from(text(v, "image", &ctx)?),
        imports: PathBuf::from(text(v, "imports", &ctx)?),
        exports: PathBuf::from(text(v, "exports", &ctx)?),
        entry,
        key_page: num(v, "key_page", &ctx)?,
        gates: PathBuf::from(text(v, "gates", &ctx)?),
        name,
    })
}

fn memory(v: &Value) -> Result<Memory> {
    let mut m = Memory::default();
    let Some(block) = v.get("memory") else {
        return Ok(m);
    };
    let read = |key: &str| -> Result<Option<u64>> {
        block
            .get(key)
            .map(|x| x.as_u64().ok_or_else(|| bad(format!("memory.{key} is not a number"))))
            .transpose()
    };
    for (key, slot) in [
        ("alias", &mut m.alias),
        ("stack", &mut m.stack),
        ("app_pool", &mut m.app_pool),
        ("mem_used", &mut m.mem_used),
        ("sys_resource", &mut m.sys_resource),
    ] {
        if let Some(x) = read(key)? {
            *slot = x;
        }
    }
    // The heap is the part of `reserve` that the alias region, the stack region and the front space leave.
    match (read("heap")?, read("reserve")?) {
        (Some(heap), _) => m.heap = heap,
        (None, Some(reserve)) => {
            m.heap = reserve
                .checked_sub(m.alias)
                .and_then(|r| r.checked_sub(m.stack))
                .and_then(|r| r.checked_sub(RESERVE_FRONT))
                .ok_or_else(|| bad("memory.reserve is smaller than alias, stack and 0x10000000"))?;
        }
        (None, None) => {}
    }
    if let Some(flag) = block.get("map_limit") {
        m.map_limit = flag.as_bool().ok_or_else(|| bad("memory.map_limit is not a boolean"))?;
    }
    Ok(m)
}

impl Game {
    /// Reads `<root>/game.json`.
    pub fn read(root: &Path) -> Result<Game> {
        let path = root.join("game.json");
        let src = std::fs::read_to_string(&path).map_err(|e| HostError::io(&path, &e))?;
        Game::parse(root, &src)
    }

    /// Parses the text of a `game.json`.
    pub fn parse(root: &Path, src: &str) -> Result<Game> {
        let v = json::parse(src)?;
        let ctx = "game.json";
        let mods = field(&v, "modules", ctx)?
            .as_array()
            .ok_or_else(|| bad("\"modules\" is not an array"))?;
        let modules = mods
            .iter()
            .enumerate()
            .map(|(i, m)| module(m, i))
            .collect::<Result<Vec<_>>>()?;
        if modules.is_empty() {
            return Err(bad("\"modules\" is empty"));
        }
        let mt = field(&v, "main_thread", ctx)?;
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
            version: v.get("version").and_then(Value::as_str).unwrap_or("").to_owned(),
            modules,
            services,
            romfs: PathBuf::from(text(&v, "romfs", ctx)?),
            patches: opt_path("patches"),
            main_thread: MainThread {
                priority: i32::try_from(
                    field(mt, "priority", "main_thread")?
                        .as_i64()
                        .ok_or_else(|| bad("main_thread.priority is not a number"))?,
                )
                .map_err(|_| bad("main_thread.priority is out of range"))?,
                core: i32::try_from(
                    field(mt, "core", "main_thread")?
                        .as_i64()
                        .ok_or_else(|| bad("main_thread.core is not a number"))?,
                )
                .map_err(|_| bad("main_thread.core is out of range"))?,
                stack_size: num(mt, "stack_size", "main_thread")?,
            },
            memory: memory(&v)?,
            npdm: opt_path("npdm"),
        };
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

    /// Checks what the loader relies on: page alignment, no overlap, areas inside the image.
    fn check(&self) -> Result<()> {
        const PAGE: u64 = 0x4000;
        for m in &self.modules {
            let n = &m.name;
            if m.base % PAGE != 0 || m.size == 0 {
                return Err(bad(format!("module {n}: base {:#x} is not page aligned or size is 0", m.base)));
            }
            if m.data.offset % PAGE != 0 {
                return Err(bad(format!("module {n}: data offset {:#x} is not page aligned", m.data.offset)));
            }
            if m.bss_offset().saturating_add(m.bss_size) > m.size {
                return Err(bad(format!("module {n}: data and bss lie outside the image")));
            }
            if m.key_page % PAGE != 0 || m.key_page.saturating_add(PAGE) > m.size {
                return Err(bad(format!("module {n}: key page {:#x} is not aligned or outside the image", m.key_page)));
            }
            if m.entry.is_some_and(|e| e >= m.size) {
                return Err(bad(format!("module {n}: entry is outside the image")));
            }
        }
        let mut spans: Vec<(u64, u64, &str)> = self.modules.iter().map(|m| (m.base, m.end(), m.name.as_str())).collect();
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
}

/// Reads an import or export list: a JSON array of `{ "name", "slot" }`.
pub fn read_slots(path: &Path) -> Result<Vec<Slot>> {
    let src = std::fs::read_to_string(path).map_err(|e| HostError::io(path, &e))?;
    parse_slots(&src).map_err(|e| bad(format!("{}: {e}", path.display())))
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

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "title_id": "01007EF00011E000", "version": "1.6.0", "services": ["kernel", "offline"],
        "romfs": "romfs", "main_thread": {"priority": 44, "core": 0, "stack_size": "0x100000"},
        "modules": [
          {"name": "main", "base": "0x100000000", "size": "0x20000", "text": {"offset": 0, "size": "0x8000"},
           "rodata": {"offset": "0x8000", "size": "0x4000"}, "data": {"offset": "0xc000", "size": "0x4000"},
           "bss_size": "0x4000", "image": "main.bin", "imports": "main.imports.json", "exports": "main.exports.json",
           "entry": "0x100", "key_page": "0x1c000", "gates": "main.gates.tsv"}
        ]}"#;

    #[test]
    fn parses_a_sample() {
        let r = Game::parse(Path::new("/x"), SAMPLE);
        assert!(r.is_ok(), "{:?}", r.as_ref().err());
        let Ok(g) = r else { return };
        assert_eq!(g.modules[0].base, 0x1_0000_0000);
        assert_eq!(g.modules[0].end(), 0x1_0002_0000);
        assert_eq!(g.main_thread.stack_size, 0x10_0000);
        assert_eq!(g.main_module().map(|m| m.name.as_str()).ok(), Some("main"));
    }

    #[test]
    fn the_heap_follows_from_reserve() {
        let s = SAMPLE.replace(
            "\"modules\"",
            "\"memory\": {\"reserve\": \"0x100000000\", \"alias\": \"0x10000000\", \"stack\": \"0x10000000\", \"app_pool\": \"0xcd500000\", \"map_limit\": true}, \"modules\"",
        );
        let r = Game::parse(Path::new("/x"), &s);
        assert!(r.is_ok(), "{:?}", r.as_ref().err());
        let Ok(g) = r else { return };
        assert_eq!(g.memory.heap, 0xd000_0000);
        assert_eq!(g.memory.app_pool, 0xcd50_0000);
        assert!(g.memory.map_limit);
        assert_eq!(g.memory.mem_used, 0x400_0000);
    }

    #[test]
    fn refuses_a_reserve_that_is_too_small() {
        let s = SAMPLE.replace("\"modules\"", "\"memory\": {\"reserve\": \"0x1000\"}, \"modules\"");
        assert!(Game::parse(Path::new("/x"), &s).is_err());
    }

    #[test]
    fn refuses_a_missing_key_page() {
        let s = SAMPLE.replace("\"key_page\": \"0x1c000\",", "");
        assert!(Game::parse(Path::new("/x"), &s).is_err());
    }

    #[test]
    fn reads_a_list() {
        let l = parse_slots(r#"[{"name": "_ZN2nn2os5SleepE", "slot": "0x40"}]"#).unwrap_or_default();
        assert_eq!(l, vec![Slot { name: "_ZN2nn2os5SleepE".into(), slot: 0x40 }]);
    }
}
