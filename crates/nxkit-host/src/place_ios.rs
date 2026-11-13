//! iOS uses dyld's signed executable mappings only. No JIT or executable remapping fallback.
use crate::memory::GuestMemory;
use crate::sys::{self, Dylib};
use crate::{Game, HostError, Result, image, layout, macho, memory, tail};
use std::io::Read;

/// How the signed guest image was placed in this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// dyld mapped the signed library and its reserved anonymous tail.
    Dylib,
}
/// The verified guest placement handed to the shared loader.
#[derive(Debug)]
pub struct Placed {
    /// Manifest adjusted to the actual dyld slide.
    pub game: Game,
    /// The mapping strategy used for this image.
    pub placement: Placement,
    /// Retained library handle; successful sessions keep it until process exit.
    pub dylib: Option<Dylib>,
    /// Registered process-lifetime guest reservation.
    pub mem: &'static GuestMemory,
    /// Module data images in manifest order.
    pub images: Vec<Vec<u8>>,
}

fn commands(game: &Game) -> Result<Vec<u8>> {
    let path = game.path(&game.dylib);
    let mut file = std::fs::File::open(&path).map_err(|e| HostError::io(&path, &e))?;
    let mut head = [0; 32];
    file.read_exact(&mut head)
        .map_err(|e| HostError::io(&path, &e))?;
    let end = macho::commands_end(&head)?;
    if !(32..=16 * 1024 * 1024).contains(&end) {
        return Err(HostError::Load("Invalid guest Mach-O command size".into()));
    }
    let mut bytes = vec![0; end];
    bytes[..32].copy_from_slice(&head);
    file.read_exact(&mut bytes[32..])
        .map_err(|e| HostError::io(&path, &e))?;
    image::validate(&bytes)?;
    Ok(bytes)
}

/// Validates the signed image contract and adopts dyld's guest reservation.
pub fn place(game: &Game) -> Result<Placed> {
    let commands = commands(game)?;
    let segs = macho::segments(&commands)?;
    for module in &game.modules {
        macho::verify(module, &segs)?;
    }
    let plan = tail::plan(game, &segs, 0)?;
    if plan.layout.all.len >= layout::SPACE_END {
        return Err(HostError::Memory(
            "Guest span cannot fit the 39-bit address space".into(),
        ));
    }
    let images = game
        .modules
        .iter()
        .map(|m| {
            let p = game.path(&m.image);
            std::fs::read(&p).map_err(|e| HostError::io(&p, &e))
        })
        .collect::<Result<Vec<_>>>()?;
    let dylib = Dylib::open(&game.path(&game.dylib))?;
    let (game, layout, res) = match sys::ios::map(game, &dylib) {
        Ok(v) => v,
        Err(e) => {
            dylib.close();
            return Err(e);
        }
    };
    // The session permits one initialization. Once registered, guest mappings and the
    // dylib live until process exit, never while detached guest threads could use them.
    let mem = memory::init(layout, res)?;
    Ok(Placed {
        game,
        placement: Placement::Dylib,
        dylib: Some(dylib),
        mem,
        images,
    })
}
