//! Shared guest initialization before AppKit or UIKit takes over presentation.
use crate::{
    Game, HostError, Result, boot::Boot, guestsym, layout::Layout, loader, place, seams, sys,
    thread,
};
use nxkit_metal::Context;
use std::{path::Path, sync::Arc};

pub(crate) struct Prepared {
    pub(crate) loaded: loader::Loaded,
    pub(crate) memory: Arc<nxkit_metal::GuestMemory>,
    pub(crate) context: Context,
    pub(crate) layout: Layout,
    pub(crate) boot: Boot,
}

// Keep this preflight before UIKit latches the process-lifetime session.
pub(crate) fn service_mask(game: &Game) -> Result<u64> {
    let mask = loader::service_mask(&game.services)?;
    if mask & nxkit_rt::Service::Kernel.bit() == 0 {
        return Err(HostError::Manifest(
            "\"services\" must include \"kernel\", because every svc of the game goes through it"
                .into(),
        ));
    }
    Ok(mask)
}

pub(crate) fn prepare(game: &Game, mask: u64, data: &Path) -> Result<Prepared> {
    let placed = place::place(game)?;
    let mem = placed.mem;
    let layout = mem.layout;
    // Metal must see the GPU buffer before sealing guard pages as PROT_NONE.
    let context = Context::system_default().map_err(|e| HostError::App(format!("Metal: {e}")))?;
    let memory = Arc::new(mem.gpu_memory(&context)?);
    mem.keep_gpu_pages();
    mem.seal()?;
    #[cfg(target_os = "macos")]
    crate::memreport::start(&context, &layout);

    nxkit_rt::nxkit_init(mask);
    sys::kernel_init(thread::guest_thread_entry, &layout, &placed.game)?;
    seams::register_kernel()?;
    seams::register_server(mask)?;
    seams::init_services_with_data(&placed.game, mask, data)?;
    let mut loaded = loader::load(placed, mask)?;
    #[cfg(target_os = "macos")]
    {
        eprintln!(
            "nxkit-host: {} modules loaded ({:?}), {} exports",
            loaded.game.modules.len(),
            loaded.placement,
            loaded.exports.len()
        );
        sys::crashsig::set_modules(
            loaded
                .game
                .modules
                .iter()
                .map(|m| {
                    let start = m.base.wrapping_add_signed(m.low);
                    (m.name.clone(), start, start + m.span)
                })
                .collect(),
        );
        nxkit_metal::set_pc_namer(sys::crashsig::guest_place);
    }
    guestsym::set(std::mem::take(&mut loaded.exports));
    let boot = Boot::resolve(std::mem::take(&mut loaded.inits))?;
    Ok(Prepared {
        loaded,
        memory,
        context,
        layout,
        boot,
    })
}
