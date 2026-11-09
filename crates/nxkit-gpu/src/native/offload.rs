//! The Neural Engine path in the frame: which picture writes which slot, which uses the previous result, and the
//! health counts of both.

use super::ane::{Session, SlotTextures, Ticket, bound, cache_root, textures};
use super::ring::Miss;
use super::run::{count_ane, count_ane_fallback};
use super::*;

/// The model of one network and set of weights.
pub(super) struct AneNet {
    session: Session,
    textures: Option<Result<Vec<SlotTextures>, String>>,
    input: (&'static str, GameResource),
    outputs: &'static [(&'static str, GameResource)],
}

/// What a picture's frame does with the Neural Engine.
pub(super) struct AnePrepared {
    pub(super) entries: Arc<AneEntries>,
    /// The slot textures this frame binds: the input to write and, with the epilogue, the outputs to read.
    pub(super) textures: Vec<(GameResource, Texture)>,
    /// The frame writes the model's input.
    pub(super) write: bool,
    /// The frame makes the output from the model's outputs; otherwise the plan runs.
    pub(super) epilogue: bool,
    /// Settled when the frame has finished on the GPU.
    pub(super) ticket: Mutex<Option<Ticket>>,
}

impl Native {
    /// Decides what the Neural Engine does for this picture and records it in `prepared`. Any problem leaves
    /// `prepared` as it is: the plan runs, counted as `<name>.ane_fallback(reason)`.
    pub(super) fn attach_ane(
        &mut self,
        net: usize,
        hash: u64,
        entry: &Arc<Entry>,
        prepared: &mut Prepared,
        cx: &PrepareCx<'_>,
    ) {
        let name = self.nets[net].name();
        let Some(entries) = entry.ane.clone() else {
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
            let (input, outputs) = (off.input_tensor, off.output_tensors);
            let session = Session::spawn(name, off, self.blobs.clone(), root, cache_key);
            self.ane.insert(
                key,
                AneNet {
                    session,
                    textures: None,
                    input,
                    outputs,
                },
            );
        }
        let Some(net_ane) = self.ane.get_mut(&key) else {
            return;
        };
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
                count_ane_fallback(name, "textures", why);
                return;
            }
            None => return,
        };
        let turn = loaded.begin();
        let mut bound_textures = Vec::new();
        if let Some(w) = turn.write {
            bound_textures.push((net_ane.input.1, slots[w].input.clone()));
        } else {
            count_ane_fallback(name, "ring", "every slot is busy, the picture is not sent");
        }
        let epilogue = match turn.read {
            Ok(r) => {
                bound_textures.extend(bound(net_ane.outputs, &slots[r]));
                count_ane(name);
                true
            }
            Err(Miss::First) => {
                count_ane_fallback(name, "first", "the first picture has no result before it");
                false
            }
            Err(Miss::Late) => {
                count_ane_fallback(name, "late", "the previous picture's result is not ready");
                false
            }
        };
        prepared.ane = Some(AnePrepared {
            entries,
            textures: bound_textures,
            write: turn.write.is_some(),
            epilogue,
            ticket: Mutex::new(Some(Ticket::new(loaded, turn))),
        });
    }
}
