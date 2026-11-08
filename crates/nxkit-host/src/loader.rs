//! The loader (R18). It finishes what `place` started: the bss, the key pages, the relocations, the import slots and
//! the gates.
//!
//! Every address here is the real one. `place` moved the modules by the slide of dyld, when there was one.
//!
//! The order per module: the file-backed part of the bss is zeroed, the key page is filled, the relocations of the NSO
//! image are applied for the real base (`reloc.rs`), the import slots are filled and the key page becomes read only.
//! The functions of the services come first for both relocations and imports, so that a service replaces an SDK
//! function of the same name (R14). A relocation of a symbol that the module defines binds to a service, else to the
//! export of the first module in load order that has the name, else to the module's own definition. An import that
//! nothing provides stops the load with its name (R15), unless it is weak, which stores 0 and is logged, or the title
//! lists it in `settings.optional_imports`. The slot of an import gets the address plus the addend. The relocation
//! plan and the import list must cover the same slots: a count that differs stops the load. Then the thread-specific
//! data is checked and the gates are installed.

use std::collections::{HashMap, HashSet};

use nxkit_rt::{entries, find_big_in, find_in};

use crate::bigstack;
use crate::error::{HostError, Result};
use crate::gate;
use crate::gates::{self, GateTable};
use crate::layout::PAGE;
use crate::manifest::{self, Game, Import, Slot};
use crate::memory::GuestMemory;
use crate::module::Module;
use crate::place::{Placed, Placement};
use crate::reloc;
use crate::seams;
use crate::sys::{self, Dylib, PROT_R};
use crate::thread::{self, Keys};

/// The value that the check of the thread-specific data layout stores and reads back.
const TSD_PROBE: u64 = 0x1234;

/// The result of the load.
#[derive(Debug)]
pub struct Loaded {
    /// The game with the real addresses.
    pub game: Game,
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
    imports: Vec<Import>,
    exports: Vec<Slot>,
}

fn read_lists(game: &Game) -> Result<Vec<Lists>> {
    game.modules
        .iter()
        .map(|m| {
            Ok(Lists {
                imports: manifest::read_imports(&game.path(&m.imports))?,
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

/// Finds what an import or a relocation needs.
struct Resolver<'a> {
    mask: u64,
    exports: &'a HashMap<String, u64>,
    big: &'a HashSet<String>,
}

impl Resolver<'_> {
    /// The function of a service. One that is in `big` is reached through a big-stack stub.
    fn service(&self, name: &str) -> Result<Option<u64>> {
        if self.big.contains(name) {
            return match find_big_in(entries(), self.mask, name.as_bytes()) {
                Some(a) => bigstack::wrap(a as u64).map(Some),
                None => Ok(None),
            };
        }
        Ok(find_in(entries(), self.mask, name.as_bytes()).map(|a| a as u64))
    }

    /// A service function, or else an export of a module.
    fn find(&self, name: &str) -> Result<Option<u64>> {
        match self.service(name)? {
            Some(a) => Ok(Some(a)),
            None => Ok(self.exports.get(name).copied()),
        }
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

/// Zeroes the part of the bss that shares file-backed pages with the data. The pages behind it come zeroed from the
/// system, in both ways of placement, so the rest is not touched and costs no memory.
fn zero_bss(mem: &GuestMemory, m: &Module) -> Result<()> {
    let start = m.bss_offset();
    let Some(seg) = m.segment_at(start) else {
        return Err(HostError::Load(format!("module {}: the bss is in no segment", m.name)));
    };
    let file_end = seg.offset.saturating_add_unsigned(seg.file_size.div_ceil(PAGE) * PAGE);
    let end = i64::try_from(start.saturating_add(m.bss_size)).unwrap_or(i64::MAX).min(file_end);
    let Ok(start_i) = i64::try_from(start) else {
        return Ok(());
    };
    if end > start_i {
        mem.res.zero(m.base + start, end.abs_diff(start_i))?;
    }
    Ok(())
}

fn fill_key_page(mem: &GuestMemory, m: &Module, keys: &Keys) -> Result<()> {
    let page = m.base + m.key_page;
    mem.res.zero(page, PAGE)?;
    for (i, k) in keys.in_page_order().iter().enumerate() {
        mem.res.write_u64(page + 8 * i as u64, 8 * *k as u64)?;
    }
    Ok(())
}

/// Applies the relocations of the image and returns how many of them name an undefined symbol.
fn apply_relocations(mem: &GuestMemory, m: &Module, image: &[u8], r: &Resolver) -> Result<usize> {
    let plan = reloc::plan(m, image, &mut |name| r.find(name))?;
    for w in &plan.writes {
        if !m.writable(w.offset, 8) {
            let seg = m.segment_at(w.offset).map_or("no segment", |s| s.name.as_str());
            return Err(HostError::Load(format!(
                "module {}: a relocation targets offset {:#x} in {seg}, which is not writable",
                m.name, w.offset
            )));
        }
        mem.res.write_u64(m.base + w.offset, w.value)?;
    }
    eprintln!(
        "nxkit-host: module {}: {} relative, {} relr, {} symbol relocations, {} left to the imports",
        m.name, plan.counts.relative, plan.counts.relr, plan.counts.symbols, plan.counts.undefined
    );
    Ok(plan.counts.undefined)
}

fn fill_imports(mem: &GuestMemory, m: &Module, imports: &[Import], r: &Resolver, optional: &[String]) -> Result<()> {
    let mut weak_missing: Vec<&str> = Vec::new();
    for s in imports {
        if s.slot % 8 != 0 || !m.writable(s.slot, 8) {
            return Err(HostError::Load(format!(
                "module {}: the import slot of {} at {:#x} is not in a writable segment",
                m.name, s.name, s.slot
            )));
        }
        let addr = match r.find(&s.name)? {
            Some(a) => a.wrapping_add_signed(s.addend),
            None if s.weak => {
                weak_missing.push(&s.name);
                0
            }
            None if optional.iter().any(|o| *o == s.name) => 0,
            None => {
                return Err(HostError::MissingImport {
                    module: m.name.clone(),
                    name: s.name.clone(),
                });
            }
        };
        mem.res.write_u64(m.base + s.slot, addr)?;
    }
    if !weak_missing.is_empty() {
        weak_missing.dedup();
        let shown = weak_missing.iter().take(8).copied().collect::<Vec<_>>().join(", ");
        eprintln!(
            "nxkit-host: module {}: {} weak imports have no provider and are 0: {shown}{}",
            m.name,
            weak_missing.len(),
            if weak_missing.len() > 8 { ", ..." } else { "" }
        );
    }
    Ok(())
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

fn load_gates(game: &Game) -> Result<GateTable> {
    let mut all = Vec::new();
    for m in &game.modules {
        let path = game.path(&m.gates);
        let src = std::fs::read_to_string(&path).map_err(|e| HostError::io(&path, &e))?;
        all.extend(gates::parse(&src, m.base)?);
    }
    GateTable::build(&all)
}

/// Finishes the load of every module, fills the imports and installs the gates.
///
/// `mask` is the set of enabled services.
pub fn load(placed: Placed, mask: u64) -> Result<Loaded> {
    let Placed {
        game,
        placement,
        dylib,
        mem,
        images,
    } = placed;
    let keys = thread::create_keys()?;
    let lists = read_lists(&game)?;
    let exports = collect_exports(&game, &lists);
    let big = seams::big_stack_exports(mask);
    let resolver = Resolver {
        mask,
        exports: &exports,
        big: &big,
    };
    for ((m, l), image) in game.modules.iter().zip(&lists).zip(&images) {
        zero_bss(mem, m)?;
        fill_key_page(mem, m, &keys)?;
        let undefined = apply_relocations(mem, m, image, &resolver)?;
        if undefined != l.imports.len() {
            return Err(HostError::Load(format!(
                "module {}: the image has {undefined} relocations of undefined symbols and the import list has {} slots",
                m.name,
                l.imports.len()
            )));
        }
        fill_imports(mem, m, &l.imports, &resolver, &game.settings.optional_imports)?;
        mem.res.protect(m.base + m.key_page, PAGE, PROT_R)?;
    }
    drop(images);
    let main = game.main_module()?;
    tsd_check(mem, main, &keys)?;
    gate::install(load_gates(&game)?)?;
    let Some(offset) = main.entry else {
        return Err(HostError::Manifest(format!("module {} has no entry", main.name)));
    };
    let entry = main.base + offset;
    Ok(Loaded {
        entry,
        exports,
        placement,
        dylib,
        game,
    })
}
