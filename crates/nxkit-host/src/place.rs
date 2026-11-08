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
//! The second way is the file map. macOS arm64 does not run code in anonymous pages, even after `mprotect` to `r-x`
//! (measured 2026-11-08), so the host maps the code from the signed library itself. It reserves the whole range. It
//! asks for the preferred address first. The kernel models a 39-bit address space, so the range must end at or below
//! `1 << 39`. When the preferred range ends higher or is not free, the host takes the address that the system gives
//! below that limit and moves the game by the difference, as it does for a slid library. It reads the load commands
//! of `nxgame.dylib`, checks the segments against `game.json`, registers the code signature with `fcntl` once and maps
//! every `r-x` segment (the text and the patch area) from the file with `MAP_FIXED` over the reservation. The data
//! segments are copied from the image files. Nothing writes into the code pages after that, and `loader.rs` refuses
//! a relocation that targets one. A missing or unsigned library is an error, because no other way can run code.
//!
//! The reservation is read and write everywhere, because Metal refuses a no-copy buffer over a `PROT_NONE` page.
//!
//! A library that does not match `game.json` is an error and never a reason to copy (R15). The reason for a fall back
//! is written to the log.

use std::fs::File;
use std::os::unix::fs::FileExt;

use crate::error::{HostError, Result};
use crate::layout::{self, Layout};
use crate::macho::{self, Segment as MachSegment};
use crate::manifest::Game;
use crate::memory::{self, GuestMemory};
use crate::module::Prot;
use crate::sys::{self, Dylib, PROT_RW, Reservation};

/// How the modules came to their addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// dyld mapped the segments, and the library may have slid.
    Dylib,
    /// The host copied the data segments and mapped the code from the signed library file.
    FileMap,
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

/// Maps read and write anonymous memory over every part of `[from, to)` that no segment of the library covers. A failure
/// gives back what it mapped.
fn fill_gaps(
    res: &Reservation,
    segs: &[MachSegment],
    slide: i64,
    from: u64,
    to: u64,
) -> Result<()> {
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
        if let Err(e) = res.map_anon(at, len, PROT_RW) {
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
        .map(|s| {
            s.vmaddr
                .wrapping_add_signed(image.slide)
                .wrapping_add(s.vmsize)
        })
        .max()
        .unwrap_or(0);
    let layout = layout::plan(&moved, floor)?;
    if !layout.fits_address_space() {
        return Ok(Err(format!(
            "the library at slide {:#x} would end at {:#x}, behind the {:#x} limit of the guest address space",
            image.slide,
            layout.all.end(),
            layout::SPACE_END
        )));
    }
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
        eprintln!(
            "nxkit-host: dyld moved the library by {:#x}, the loader relocates the modules",
            image.slide
        );
    }
    Ok(Ok(Mapped {
        game: moved,
        layout,
        res,
    }))
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

/// Reserves the whole guest range below `SPACE_END` and returns the game moved to where the system gave it, with its
/// layout. The preferred address is tried first, if its range fits. Then the address that the system gives below the
/// limit is tried. The slide is a multiple of `REGION_ALIGN`, so the regions keep the alignment that the layout needs.
pub fn reserve_guest(game: &Game) -> Result<(Reservation, Game, Layout)> {
    let wanted = layout::plan(game, 0)?;
    let (start, len) = (wanted.all.start, wanted.all.len);
    let mut hints = Vec::new();
    if wanted.fits_address_space() {
        hints.push(start);
    }
    hints.push(layout::FALLBACK_BASE + start % layout::REGION_ALIGN);
    let accept = |got: u64| {
        got.checked_add(len).is_some_and(|e| e <= layout::SPACE_END)
            && got.wrapping_sub(start) % layout::REGION_ALIGN == 0
    };
    let res = sys::reserve_where(&hints, len, accept).map_err(|e| {
        HostError::Memory(format!(
            "the guest range {start:#x} + {len:#x} has no home below {:#x}: {e}",
            layout::SPACE_END
        ))
    })?;
    let slide = res.base().wrapping_sub(start) as i64;
    if slide == 0 {
        return Ok((res, game.clone(), wanted));
    }
    let moved = game.moved(slide)?;
    let layout = layout::plan(&moved, 0)?;
    if layout.all.start != res.base() || layout.all.len != len {
        return Err(HostError::Memory(format!(
            "the guest range moved to {:#x} changes its size from {len:#x} to {:#x}",
            res.base(),
            layout.all.len
        )));
    }
    eprintln!(
        "nxkit-host: the guest moved by {slide:#x} to {:#x}",
        res.base()
    );
    Ok((res, moved, layout))
}

/// The library file, its segments and where its code signature is.
struct CodeFile {
    file: File,
    segs: Vec<MachSegment>,
    sig: (u64, u64),
}

/// Opens the library, reads its load commands and checks every segment of every module against `game.json`. A
/// library that is missing, not signed or different from `game.json` is an error (R15).
fn open_code_file(game: &Game) -> Result<CodeFile> {
    let path = game.path(&game.dylib);
    let missing = |e: std::io::Error| {
        HostError::Load(format!(
            "{}: {e}. The code of the game is mapped from this signed library, and nothing else can run it",
            path.display()
        ))
    };
    let file = File::open(&path).map_err(missing)?;
    let mut head = [0u8; macho::HEADER_SIZE];
    file.read_exact_at(&mut head, 0).map_err(missing)?;
    let mut cmds = vec![0u8; macho::commands_end(&head)?];
    file.read_exact_at(&mut cmds, 0).map_err(missing)?;
    let segs = macho::segments(&cmds)?;
    for m in &game.modules {
        macho::verify(m, &segs)?;
    }
    let Some((off, size)) = macho::code_signature(&cmds)? else {
        return Err(HostError::Load(format!(
            "{} has no code signature. Sign it with `codesign -s -`, because macOS runs only signed code",
            path.display()
        )));
    };
    Ok(CodeFile {
        file,
        segs,
        sig: (u64::from(off), u64::from(size)),
    })
}

fn via_file_map(game: &Game, images: &[Vec<u8>]) -> Result<Mapped> {
    let code = open_code_file(game)?;
    sys::add_file_signature(&code.file, code.sig.0, code.sig.1).map_err(|e| {
        HostError::Load(format!(
            "{}: the system does not take the code signature: {e}",
            game.dylib.display()
        ))
    })?;
    let (res, game, layout) = reserve_guest(game)?;
    for (m, image) in game.modules.iter().zip(images) {
        for s in &m.segments {
            let at = m.segment_addr(s);
            if s.prot == Prot::Exec {
                let found = code.segs.iter().find(|x| x.name == s.name).ok_or_else(|| {
                    HostError::Load(format!(
                        "module {}: the library has no segment {}",
                        m.name, s.name
                    ))
                })?;
                if found.filesize > 0 {
                    res.map_file_exec(at, found.filesize, &code.file, found.fileoff)?;
                }
                continue;
            }
            let bytes = usize::try_from(s.file_offset)
                .ok()
                .zip(usize::try_from(s.file_offset + s.file_size).ok())
                .and_then(|(a, b)| image.get(a..b))
                .ok_or_else(|| {
                    HostError::Load(format!(
                        "module {}: segment {} lies outside the image file",
                        m.name, s.name
                    ))
                })?;
            res.copy_in(at, bytes)?;
        }
    }
    Ok(Mapped { game, layout, res })
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
    eprintln!(
        "nxkit-host: not using {} ({reason}), copying the data segments and mapping the code from {} instead",
        game.dylib.display(),
        game.dylib.display()
    );
    let m = via_file_map(game, &images)?;
    let mem = memory::init(m.layout, m.res)?;
    Ok(Placed {
        game: m.game,
        placement: Placement::FileMap,
        dylib: None,
        mem,
        images,
    })
}
