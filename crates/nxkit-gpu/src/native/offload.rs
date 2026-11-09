//! The Neural Engine path in the frame: which picture's result the game's dispatch uses, and the health counts of it.
//!
//! The model runs ahead of the game ([`super::feed`]). When the dispatch of a picture is prepared, the picture is found
//! by the guest memory its planes lie in; if its result is ready the frame runs the epilogue in place of the plan,
//! otherwise the plan runs, as it does without the Neural Engine path. Either way no pass is added.

use super::ane::{Session, SlotTextures, bound, cache_root, textures};
use super::conv::Params;
use super::feed::{Hold, feed};
use super::iface::PictureSpec;
use super::run::{count_ane, count_ane_fallback};
use super::*;

/// The model of one network and set of weights.
pub(super) struct AneNet {
    session: Session,
    textures: Option<Result<Vec<SlotTextures>, String>>,
    outputs: &'static [(&'static str, GameResource)],
    picture: PictureSpec,
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
        let host = |r: GameResource| {
            let (_, gpu) = prepared.addrs.iter().find(|(g, _)| *g == r)?;
            Some(cx.guest.host_base() + cx.guest_offset(*gpu, 1)?)
        };
        let (Some(luma), Some(chroma)) = (host(spec.luma), host(spec.chroma)) else {
            count_ane_fallback(
                name,
                "unknown_picture",
                "the picture's planes do not lie in guest memory",
            );
            return;
        };
        let hold = match feed.pick(epoch, luma, chroma) {
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
