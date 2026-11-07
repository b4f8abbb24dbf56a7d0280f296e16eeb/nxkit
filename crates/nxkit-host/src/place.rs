//! Placement (R18): the modules go into the one reservation of guest memory.
//!
//! The first way is `dlopen` of `nxgame.dylib`. The build gives every module the segments `__NX_<NAME>_TEXT`,
//! `_DATA` and `_PATCH` at the preferred addresses of `game.json`. dyld treats them as preferred only and may slide
//! the whole library. The host takes the slide, reserves the rest of the guest range behind what dyld mapped and
//! fills the gaps between the segments. All addresses of the game move by the slide, and the loader applies the
//! relocations for the real base (`loader.rs`, `reloc.rs`).
//!
//! The segments of the library are signed with the library, so no run time permission for unsigned code is needed.
//!
//! The second way is the copy. When the library cannot be used (dyld refuses it, or the range behind it is taken), the
//! host reserves the whole range at the preferred addresses and copies each segment from the flat image file. This is
//! what nxnative does: it copies code into anonymous memory and calls `mprotect` to make it executable. It uses no
//! `MAP_JIT` and signs nothing, which works while the process has no hardened runtime. A hardened runtime needs the
//! entitlement `com.apple.security.cs.allow-unsigned-executable-memory`, and the build of the host adds it then.
//!
//! A library that does not match `game.json` is an error and never a reason to copy (R15). The reason for a fall back
//! is written to the log.

use crate::error::{HostError, Result};
use crate::layout::{self, Layout};
use crate::macho::{self, Segment as MachSegment};
use crate::manifest::Game;
use crate::memory::{self, GuestMemory};
use crate::module::Prot;
use crate::sys::{self, Dylib, PROT_NONE, PROT_RW, PROT_RX, Reservation};

/// How the modules came to their addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// dyld mapped the segments, and the library may have slid.
    Dylib,
    /// The host copied the segments.
    Copy,
}

/// The result of the placement.
#[derive(Debug)]
pub struct Placed {
    /// The game with the addresses where the modules really sit.
    pub game: Game,
    /// How the modules were placed.
    pub placement: Placement,
    /// The library, when dyld placed the modules. It stays open for the life of the process.
    pub dylib: Option<Dylib>,
    /// The reservation.
    pub mem: &'static GuestMemory,
    /// The image file of each module, in the order of `game.modules`.
    pub images: Vec<Vec<u8>>,
}

struct Mapped {
    game: Game,
    layout: Layout,
    res: Reservation,
}

fn read_images(game: &Game) -> Result<Vec<Vec<u8>>> {
    game.modules
        .iter()
        .map(|m| {
            let path = game.path(&m.image);
            std::fs::read(&path).map_err(|e| HostError::io(&path, &e))
        })
        .collect()
}

/// Maps `PROT_NONE` anonymous memory over every part of `[from, to)` that no segment of the library covers. A failure
/// gives back what it mapped.
fn fill_gaps(res: &Reservation, segs: &[MachSegment], slide: i64, from: u64, to: u64) -> Result<()> {
    let mut spans: Vec<(u64, u64)> = segs
        .iter()
        .map(|s| {
            let at = s.vmaddr.wrapping_add_signed(slide);
            (at, at.wrapping_add(s.vmsize))
        })
        .collect();
    spans.sort_unstable();
    let mut gaps = Vec::new();
    let mut cur = from;
    for (s, e) in spans {
        if s > cur {
            gaps.push((cur, s.min(to) - cur));
        }
        cur = cur.max(e);
        if cur >= to {
            break;
        }
    }
    if cur < to {
        gaps.push((cur, to - cur));
    }
    let mut done: Vec<(u64, u64)> = Vec::new();
    for (at, len) in gaps {
        if let Err(e) = res.map_anon(at, len, PROT_NONE) {
            for (a, l) in done {
                let _ = res.unmap(a, l);
            }
            return Err(e);
        }
        done.push((at, len));
    }
    Ok(())
}

/// Tries the library. The outer error is a library that does not match `game.json`. The inner error is a reason to
/// copy instead, and then nothing of the attempt is left mapped except the library, which the caller closes.
fn attempt(game: &Game, dylib: &Dylib) -> Result<std::result::Result<Mapped, String>> {
    let image = match dylib.image() {
        Ok(i) => i,
        Err(e) => return Ok(Err(e.to_string())),
    };
    let cmds = match image.commands() {
        Ok(c) => c,
        Err(e) => return Ok(Err(e.to_string())),
    };
    let segs = macho::segments(&cmds)?;
    for m in &game.modules {
        macho::verify(m, &segs)?;
    }
    let moved = game.moved(image.slide)?;
    let floor = segs
        .iter()
        .map(|s| s.vmaddr.wrapping_add_signed(image.slide).wrapping_add(s.vmsize))
        .max()
        .unwrap_or(0);
    let layout = layout::plan(&moved, floor)?;
    let tail_len = layout.all.end().saturating_sub(floor);
    let res = match sys::reserve_tail(layout.all.start, floor, layout.all.end()) {
        Ok(r) => r,
        Err(e) => return Ok(Err(e.to_string())),
    };
    if let Err(e) = fill_gaps(&res, &segs, image.slide, layout.all.start, floor) {
        let _ = res.unmap(floor, tail_len);
        return Ok(Err(e.to_string()));
    }
    if image.slide != 0 {
        eprintln!("nxkit-host: dyld moved the library by {:#x}, the loader relocates the modules", image.slide);
    }
    Ok(Ok(Mapped { game: moved, layout, res }))
}

fn via_dylib(game: &Game) -> Result<std::result::Result<(Mapped, Dylib), String>> {
    let dylib = match Dylib::open(&game.path(&game.dylib)) {
        Ok(d) => d,
        Err(e) => return Ok(Err(e.to_string())),
    };
    match attempt(game, &dylib) {
        Ok(Ok(m)) => Ok(Ok((m, dylib))),
        Ok(Err(reason)) => {
            dylib.close();
            Ok(Err(reason))
        }
        Err(e) => {
            dylib.close();
            Err(e)
        }
    }
}

fn via_copy(game: &Game, images: &[Vec<u8>]) -> Result<Mapped> {
    let layout = layout::plan(game, 0)?;
    let res = sys::reserve(layout.all.start, layout.all.len)?;
    for (m, image) in game.modules.iter().zip(images) {
        for s in &m.segments {
            let at = m.segment_addr(s);
            let bytes = usize::try_from(s.file_offset)
                .ok()
                .zip(usize::try_from(s.file_offset + s.file_size).ok())
                .and_then(|(a, b)| image.get(a..b))
                .ok_or_else(|| {
                    HostError::Load(format!("module {}: segment {} lies outside the image file", m.name, s.name))
                })?;
            res.protect(at, s.size, PROT_RW)?;
            res.copy_in(at, bytes)?;
            if s.prot == Prot::Exec {
                res.protect(at, s.size, PROT_RX)?;
            }
        }
    }
    Ok(Mapped {
        game: game.clone(),
        layout,
        res,
    })
}

/// Puts the modules into the reservation and registers the reservation with `nxkit_abi::guest`.
///
/// `game` has the preferred addresses of `game.json`. The result has the real ones.
pub fn place(game: &Game) -> Result<Placed> {
    let images = read_images(game)?;
    let reason = match via_dylib(game)? {
        Ok((m, dylib)) => {
            let mem = memory::init(m.layout, m.res)?;
            return Ok(Placed {
                game: m.game,
                placement: Placement::Dylib,
                dylib: Some(dylib),
                mem,
                images,
            });
        }
        Err(reason) => reason,
    };
    eprintln!("nxkit-host: not using {} ({reason}), copying the segments instead", game.dylib.display());
    let m = via_copy(game, &images)?;
    let mem = memory::init(m.layout, m.res)?;
    Ok(Placed {
        game: m.game,
        placement: Placement::Copy,
        dylib: None,
        mem,
        images,
    })
}
