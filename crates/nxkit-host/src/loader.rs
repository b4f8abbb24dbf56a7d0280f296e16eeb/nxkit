//! The loader (R18). It puts the rewritten modules at their fixed addresses inside the reservation.
//!
//! The first way is `dlopen` of `nxgame.dylib`. The build gives every module two segments, `__NX_<NAME>_TEXT` and
//! `__NX_<NAME>_DATA`, with the preferred addresses of `game.json`. The module ranges are handed back to the system
//! before `dlopen`, so dyld can map the segments there. The loader then checks where they landed.
//!
//! The second way is the copy fallback. When a segment is elsewhere, the loader closes the library and maps the
//! module ranges itself from the flat `image` files. This is the same as nxnative does: it copies code into anonymous
//! memory and uses `mprotect` to make it executable. It uses no `MAP_JIT` and signs nothing. That works while the
//! process has no hardened runtime. A hardened runtime needs the entitlement
//! `com.apple.security.cs.allow-unsigned-executable-memory`, and the build of the host must add it then.
//!
//! The loader then zeroes the bss, fills the key page, fills the import slots and installs the gates.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use nxkit_rt::{entries, find_in};

use crate::bigstack;
use crate::error::{HostError, Result};
use crate::gate;
use crate::gates::{self, GateTable};
use crate::layout::PAGE;
use crate::macho::{self, Segment};
use crate::manifest::{self, Game, Module, Slot};
use crate::memory::GuestMemory;
use crate::seams;
use crate::sys::{self, Dylib, PROT_NONE, PROT_R, PROT_RW, PROT_RX};
use crate::thread::{self, Keys};

/// The name of the library in the run folder.
pub const DYLIB_NAME: &str = "nxgame.dylib";
/// The value that the check of the thread-specific data layout stores and reads back.
const TSD_PROBE: u64 = 0x1234;

/// How the modules came to their addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// dyld mapped the segments at the preferred addresses.
    Dylib,
    /// The loader copied the images.
    Copy,
}

/// The result of the load.
#[derive(Debug)]
pub struct Loaded {
    /// How the modules were placed.
    pub placement: Placement,
    /// The address of the entry point of the main module.
    pub entry: u64,
    /// Every export of every module, by Switch mangled name, as a guest address.
    pub exports: HashMap<String, u64>,
    /// The library, when dyld placed the modules. It stays open for the life of the process.
    pub dylib: Option<Dylib>,
}

struct Lists {
    imports: Vec<Slot>,
    exports: Vec<Slot>,
}

fn span(m: &Module) -> u64 {
    (m.size + PAGE - 1) & !(PAGE - 1)
}

fn read_lists(game: &Game) -> Result<Vec<Lists>> {
    game.modules
        .iter()
        .map(|m| {
            Ok(Lists {
                imports: manifest::read_slots(&game.path(&m.imports))?,
                exports: manifest::read_slots(&game.path(&m.exports))?,
            })
        })
        .collect()
}

fn collect_exports(game: &Game, lists: &[Lists]) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    for (m, l) in game.modules.iter().zip(lists) {
        for s in &l.exports {
            out.entry(s.name.clone()).or_insert(m.base + s.slot);
        }
    }
    out
}

/// Finds what an import needs. The functions of the services come first, so that a service replaces an SDK function
/// of the same name (R14). The exports of the modules come next. A service function that is in `big` is reached
/// through a big-stack stub.
struct Resolver<'a> {
    mask: u64,
    exports: &'a HashMap<String, u64>,
    big: &'a HashSet<String>,
}

impl Resolver<'_> {
    fn find(&self, name: &str) -> Result<Option<u64>> {
        if let Some(a) = find_in(entries(), self.mask, name.as_bytes()) {
            let a = a as u64;
            return if self.big.contains(name) { bigstack::wrap(a).map(Some) } else { Ok(Some(a)) };
        }
        Ok(self.exports.get(name).copied())
    }
}

/// The index of a service name of `game.json` in the mask of `nxkit-rt`.
pub fn service_mask(names: &[String]) -> Result<u64> {
    let mut mask = 0;
    for n in names {
        let found = nxkit_rt::Service::ALL
            .iter()
            .find(|s| format!("{s:?}").eq_ignore_ascii_case(n));
        match found {
            Some(s) => mask |= s.bit(),
            None => return Err(HostError::Manifest(format!("unknown service \"{n}\""))),
        }
    }
    Ok(mask)
}

fn find_segment<'a>(segs: &'a [Segment], name: &str) -> Result<&'a Segment> {
    segs.iter()
        .find(|s| s.name == name)
        .ok_or_else(|| HostError::Load(format!("{DYLIB_NAME} has no segment {name}")))
}

/// The two ranges of a module as dyld mapped them, or `None` when they are not at the base of the module.
fn dyld_ranges(segs: &[Segment], slide: u64, m: &Module) -> Result<Option<[(u64, u64); 2]>> {
    let t = find_segment(segs, &macho::text_segment(&m.name))?;
    let d = find_segment(segs, &macho::data_segment(&m.name))?;
    if d.initprot & (macho::PROT_READ | macho::PROT_WRITE) != macho::PROT_READ | macho::PROT_WRITE {
        return Err(HostError::Load(format!("segment {} is not readable and writable", d.name)));
    }
    if t.initprot & macho::PROT_EXEC == 0 || t.initprot & macho::PROT_WRITE != 0 {
        return Err(HostError::Load(format!("segment {} is not read and execute only", t.name)));
    }
    let t_at = t.vmaddr.wrapping_add(slide);
    let d_at = d.vmaddr.wrapping_add(slide);
    if t_at != m.base || d_at != m.base + m.data.offset {
        return Ok(None);
    }
    Ok(Some([(t_at, t_at + t.vmsize), (d_at, d_at + d.vmsize)]))
}

fn slide_of(head: u64, segs: &[Segment]) -> Result<u64> {
    let first = segs
        .iter()
        .find(|s| s.fileoff == 0 && s.filesize > 0)
        .ok_or_else(|| HostError::Load("no segment holds the Mach-O header".into()))?;
    Ok(head.wrapping_sub(first.vmaddr))
}

/// Maps anonymous memory over the parts of a module that dyld did not cover. These are the gap between the two
/// segments and the tail after the data, which holds the bss and the key page.
fn fill_holes(mem: &GuestMemory, m: &Module, covered: &[(u64, u64); 2]) -> Result<()> {
    let end = m.base + span(m);
    let mut cur = m.base;
    for &(s, e) in covered {
        if s > cur {
            mem.res.map_anon(cur, s - cur, PROT_RW)?;
        }
        cur = cur.max(e);
    }
    if cur < end {
        mem.res.map_anon(cur, end - cur, PROT_RW)?;
    }
    Ok(())
}

fn copy_module(game: &Game, mem: &GuestMemory, m: &Module) -> Result<()> {
    let path = game.path(&m.image);
    let bytes = std::fs::read(&path).map_err(|e| HostError::io(&path, &e))?;
    if bytes.len() as u64 > span(m) {
        return Err(HostError::Load(format!(
            "image of module {} has {:#x} bytes and the module has {:#x}",
            m.name,
            bytes.len(),
            m.size
        )));
    }
    mem.res.map_anon(m.base, span(m), PROT_RW)?;
    mem.res.copy_in(m.base, &bytes)
}

fn fill_key_page(mem: &GuestMemory, m: &Module, keys: &Keys) -> Result<()> {
    let page = m.base + m.key_page;
    mem.res.zero(page, PAGE)?;
    for (i, k) in keys.in_page_order().iter().enumerate() {
        mem.res.write_u64(page + 8 * i as u64, 8 * *k as u64)?;
    }
    Ok(())
}

fn fill_imports(mem: &GuestMemory, m: &Module, imports: &[Slot], r: &Resolver) -> Result<()> {
    for s in imports {
        if s.slot < m.data.offset || s.slot % 8 != 0 || s.slot + 8 > m.size {
            return Err(HostError::Load(format!(
                "module {}: the import slot of {} at {:#x} is not in the data of the module",
                m.name, s.name, s.slot
            )));
        }
        let Some(addr) = r.find(&s.name)? else {
            return Err(HostError::MissingImport {
                module: m.name.clone(),
                name: s.name.clone(),
            });
        };
        mem.res.write_u64(m.base + s.slot, addr)?;
    }
    Ok(())
}

fn finish(mem: &GuestMemory, m: &Module, keys: &Keys, imports: &[Slot], r: &Resolver, copied: bool) -> Result<()> {
    mem.res.zero(m.base + m.bss_offset(), m.bss_size)?;
    fill_key_page(mem, m, keys)?;
    fill_imports(mem, m, imports, r)?;
    if copied {
        mem.res.protect(m.base, m.data.offset, PROT_RX)?;
    }
    mem.res.protect(m.base + m.key_page, PAGE, PROT_R)
}

/// Checks that the thread-specific data of this host thread has the layout that the key page assumes.
fn tsd_check(mem: &GuestMemory, m: &Module, keys: &Keys) -> Result<()> {
    sys::key_set(keys.k18, TSD_PROBE)?;
    let off = mem.res.read_u64(m.base + m.key_page)?;
    let seen = gate::tsd_word(off);
    sys::key_set(keys.k18, 0)?;
    if seen != TSD_PROBE {
        return Err(HostError::Load(format!(
            "the thread-specific data does not match the key page: stored {TSD_PROBE:#x}, read {seen:#x}"
        )));
    }
    Ok(())
}

/// Gives the gaps between the modules back to the reservation as ranges without access.
fn seal_gaps(mem: &GuestMemory, game: &Game) -> Result<()> {
    let mut spans: Vec<(u64, u64)> = game.modules.iter().map(|m| (m.base, m.base + span(m))).collect();
    spans.sort_unstable();
    let mut cur = mem.layout.modules.start;
    for (s, e) in spans {
        if s > cur {
            mem.res.map_anon(cur, s - cur, PROT_NONE)?;
        }
        cur = cur.max(e);
    }
    Ok(())
}

fn load_gates(game: &Game) -> Result<GateTable> {
    let mut all = Vec::new();
    for m in &game.modules {
        let path = game.path(&m.gates);
        let src = std::fs::read_to_string(&path).map_err(|e| HostError::io(&path, &e))?;
        all.extend(gates::parse(&src, m.base)?);
    }
    GateTable::build(&all)
}

/// Loads every module of the game, fills the imports and installs the gates.
///
/// `mask` is the set of enabled services. The reservation must exist.
pub fn load(game: &Game, mem: &GuestMemory, mask: u64) -> Result<Loaded> {
    let keys = thread::create_keys()?;
    let lists = read_lists(game)?;
    let exports = collect_exports(game, &lists);
    let big = seams::big_stack_exports();
    let resolver = Resolver {
        mask,
        exports: &exports,
        big: &big,
    };
    for m in &game.modules {
        mem.res.unmap(m.base, span(m))?;
    }
    let dylib = Dylib::open(&game.path(Path::new(DYLIB_NAME)))?;
    let (head, bytes) = dylib.header()?;
    let segs = macho::segments(&bytes)?;
    let slide = slide_of(head, &segs)?;
    let mut covered = Vec::new();
    for m in &game.modules {
        covered.push(dyld_ranges(&segs, slide, m)?);
    }
    let placed = covered.iter().all(Option::is_some);
    let (placement, dylib) = if placed {
        for (m, c) in game.modules.iter().zip(&covered) {
            if let Some(c) = c {
                fill_holes(mem, m, c)?;
            }
        }
        (Placement::Dylib, Some(dylib))
    } else {
        let wrong: Vec<&str> = game
            .modules
            .iter()
            .zip(&covered)
            .filter(|(_, c)| c.is_none())
            .map(|(m, _)| m.name.as_str())
            .collect();
        eprintln!(
            "nxkit-host: dyld did not map the segments of {} at their addresses, copying the images instead",
            wrong.join(", ")
        );
        dylib.close();
        for m in &game.modules {
            copy_module(game, mem, m)?;
        }
        (Placement::Copy, None)
    };
    let copied = placement == Placement::Copy;
    for (m, l) in game.modules.iter().zip(&lists) {
        finish(mem, m, &keys, &l.imports, &resolver, copied)?;
    }
    seal_gaps(mem, game)?;
    let main = game.main_module()?;
    tsd_check(mem, main, &keys)?;
    gate::install(load_gates(game)?)?;
    let Some(offset) = main.entry else {
        return Err(HostError::Manifest(format!("module {} has no entry", main.name)));
    };
    let entry = main.base + offset;
    Ok(Loaded {
        placement,
        entry,
        exports,
        dylib,
    })
}
