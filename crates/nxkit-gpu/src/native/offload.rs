//! The Neural Engine path in the frame: which picture's result the game's dispatch uses, and the health counts of it.
//!
//! The model runs ahead of the game ([`super::feed`]). When the dispatch of a picture is prepared, the picture is found
//! by the guest memory its planes lie in, or, as the game copies the decoder's planes into textures first, by the
//! memory its copies took the luma from ([`super::trace`]), or by the fingerprint of the luma ([`super::print`]). If its
//! result is ready the frame runs the epilogue in place of the plan, otherwise the plan runs, as it does without the
//! Neural Engine path. Either way no pass is added.

use super::ane::{Session, SlotTextures, bound, cache_root, textures};
use super::ane_trace;
use super::conv::Params;
use super::feed::{Hold, feed};
use super::iface::PictureSpec;
use super::print::{self, Layout, Print};
use super::run::{count_ane, count_ane_fallback};
use super::trace::Hop;
use super::*;
use nxkit_nvn::tic::{Tic, TicLayout};

/// The model of one network and set of weights.
pub(super) struct AneNet {
    session: Session,
    textures: Option<Result<Vec<SlotTextures>, String>>,
    outputs: &'static [(&'static str, GameResource)],
    picture: PictureSpec,
}

/// Where the storage of a texture of the run lies and how: the GPU address, the layout and the size in texels.
pub(super) struct Placed {
    resource: GameResource,
    addr: u64,
    layout: TicLayout,
    size: (u32, u32),
}

impl Placed {
    pub(super) fn of(resource: GameResource, header: &Tic) -> Self {
        Self {
            resource,
            addr: header.address.get(),
            layout: header.layout,
            size: (header.width, header.height),
        }
    }
}

/// What a picture's frame does with the Neural Engine: it makes the output from the model's outputs.
pub(super) struct AnePrepared {
    /// The epilogue, in place of the plan.
    pub(super) entry: Arc<Entry>,
    /// The slot textures this frame binds: the outputs to read.
    pub(super) textures: Vec<(GameResource, Texture)>,
    /// The claim on the result, released when the frame has finished on the GPU.
    pub(super) hold: Mutex<Option<Hold>>,
}

impl Native {
    /// Decides whether the Neural Engine's result of this picture is used and records it in `prepared`. Any problem
    /// leaves `prepared` as it is: the plan runs, counted as `<name>.ane_fallback(reason)`.
    pub(super) fn attach_ane(
        &mut self,
        net: usize,
        hash: u64,
        entry: &Arc<Entry>,
        prepared: &mut Prepared,
        cx: &PrepareCx<'_>,
    ) {
        let name = self.nets[net].name();
        if let Some(off) = self.nets[net].offload() {
            ane_trace::dispatch(self.nets[net].resources(), off.picture, prepared, cx);
        }
        let Some(epilogue) = entry.epilogue.clone() else {
            return;
        };
        let key = (net, hash);
        if !self.ane.contains_key(&key) {
            let Some(off) = self.nets[net].offload() else {
                return;
            };
            let refs: Vec<&[u8]> = self.blobs.iter().map(Vec::as_slice).collect();
            let cache_key = (off.cache_key)(&refs, &nxkit_coreml::os_build_tag());
            let root = cache_root(
                name,
                std::env::var(util::env_name(name, "_ANE_CACHE"))
                    .ok()
                    .as_deref(),
                std::env::var("HOME").ok().as_deref(),
            );
            let (outputs, picture) = (off.output_tensors, off.picture);
            let session = Session::spawn(name, off, self.blobs.clone(), root, cache_key);
            self.ane.insert(
                key,
                AneNet {
                    session,
                    textures: None,
                    outputs,
                    picture,
                },
            );
        }
        let Some(net_ane) = self.ane.get_mut(&key) else {
            return;
        };
        let feed = feed();
        feed.attach(hash, &net_ane.session);
        if feed.given_up() {
            count_ane_fallback(
                name,
                "no_match",
                "no dispatch of this stream matched a decoded picture; the Neural Engine path is off until the next stream",
            );
            return;
        }
        // The constants come first: the pictures the decoder has next are converted with them.
        let spec = net_ane.picture;
        let Some(params) = prepared
            .pushes
            .iter()
            .find(|(r, _)| *r == spec.convert)
            .and_then(|(_, b)| Params::from_push(b))
        else {
            count_ane_fallback(
                name,
                "params",
                "the dispatch pushed no colour conversion constants",
            );
            return;
        };
        let epoch = feed.note_params(params);
        let loaded = match net_ane.session.get() {
            Ok(l) => l,
            Err(None) => {
                count_ane_fallback(name, "warmup", "the model is still loading");
                return;
            }
            Err(Some(why)) => {
                count_ane_fallback(name, "error", &why);
                return;
            }
        };
        if net_ane.textures.is_none() {
            net_ane.textures = Some(textures(cx.ctx, &loaded));
        }
        let slots = match &net_ane.textures {
            Some(Ok(t)) => t,
            Some(Err(why)) => {
                count_ane_fallback(name, "error", why);
                return;
            }
            None => return,
        };
        let host = |gpu: u64| Some(cx.guest.host_base() + cx.guest_offset(gpu, 1)?);
        let Some(luma) = prepared.addrs.iter().find(|p| p.resource == spec.luma) else {
            count_ane_fallback(name, "unknown_picture", "the dispatch binds no luma plane");
            return;
        };
        let chain = self.copies.chain(luma.addr);
        // The plane itself first (0 matches nothing when it is not in guest memory), then where it was copied from.
        let mut lumas = vec![host(luma.addr).unwrap_or(0)];
        lumas.extend(chain.iter().filter_map(|h| host(h.addr)));
        let hold = match feed.pick(epoch, &lumas, || fingerprints(cx, luma, &chain)) {
            Ok(h) => h,
            Err((reason, what)) => {
                count_ane_fallback(name, reason, &what);
                return;
            }
        };
        count_ane(name);
        prepared.ane = Some(AnePrepared {
            entry: epilogue,
            textures: bound(net_ane.outputs, &slots[hold.slot()]),
            hold: Mutex::new(Some(hold)),
        });
    }
}

/// The fingerprints of what may hold the picture's luma now: the memory the game copied the plane from (read as the
/// copy lays it out, whatever the pitch) and the plane's own storage (read through the texture's layout, for a game that
/// writes the plane with the CPU). Memory that is not in guest memory or too short gives none.
fn fingerprints(cx: &PrepareCx<'_>, luma: &Placed, chain: &[Hop]) -> Vec<Print> {
    let byte = |base: u64| {
        move |o: u64| {
            let off = cx.guest_offset(base.checked_add(o)?, 1)?;
            let mut b = [0u8];
            cx.guest.read(off, &mut b).ok()?;
            Some(b[0])
        }
    };
    let mut out = Vec::new();
    for h in chain.iter().filter(|h| !h.texture) {
        let size = if h.size[0] > 0 && h.size[1] > 0 {
            (h.size[0], h.size[1])
        } else {
            luma.size
        };
        let pitch = if h.row_stride > 0 {
            h.row_stride
        } else {
            u64::from(size.0)
        };
        out.extend(print::of_memory(size, Layout::Pitch(pitch), byte(h.addr)));
    }
    let own = match luma.layout {
        TicLayout::Pitch { stride } => Some(Layout::Pitch(u64::from(stride))),
        TicLayout::BlockLinear { tile_h, .. } => Some(Layout::Block(u32::from(tile_h))),
        TicLayout::Buffer => None,
    };
    if let Some(layout) = own {
        out.extend(print::of_memory(luma.size, layout, byte(luma.addr)));
    }
    out
}
