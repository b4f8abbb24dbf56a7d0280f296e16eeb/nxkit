//! Vertex stage stores in the middle of a render pass.
//!
//! A draw with rasterizer discard runs only the stores of its vertex stage (`main0_discard`). Xenoblade Chronicles 3
//! blends the bone matrices of an object so, into slots that the next object's blend overwrites later in the same
//! pass: matrices of X, the draws of X, matrices of Y over the same slots, the draws of Y. The console runs the draws of
//! a pass in order. Metal overlaps the vertex stages of the draws of one render pass, and a barrier from the vertex
//! stage to the vertex stage inside a pass does not order them: the draws of X read the matrices of Y, and their parts
//! spread over the screen as long spikes.
//!
//! So a run of such draws leaves the render pass: the pass ends before it (its attachments are stored), the run draws
//! in a pass of its own without attachments, and the pass goes on after it (its attachments are loaded), each behind a
//! barrier from the earlier work.

use nxkit_metal::{Dependency, LoadAction, RenderPassDesc, Scope, Stages, StoreAction, Visibility};

use super::EncodeCtx;
use crate::detail::DrawDetail;

/// A draw that only stores from its vertex stage.
fn stores(d: &DrawDetail) -> bool {
    d.raster.discard && d.capture.is_none() && !d.args.is_mesh()
}

/// Whether draw `n` only stores from its vertex stage.
pub(super) fn is_store(sh: &EncodeCtx<'_>, n: u32) -> bool {
    sh.detail.draws.get(n as usize).is_some_and(|d| stores(d))
}

/// The end of the run of store draws that starts at `n`, at most `end`.
pub(super) fn run_end(sh: &EncodeCtx<'_>, n: u32, end: u32) -> u32 {
    let mut m = n;
    while m < end && is_store(sh, m) {
        m += 1;
    }
    m
}

/// Whether the draws `range` need splitting: a run of store draws with other draws around it.
pub(super) fn needed(sh: &EncodeCtx<'_>, range: std::ops::Range<u32>) -> bool {
    let (mut store, mut other) = (false, false);
    for n in range {
        match sh.detail.draws.get(n as usize) {
            Some(d) if stores(d) => store = true,
            Some(d) if d.capture.is_none() => other = true,
            _ => {}
        }
    }
    store && other
}

/// The pass description for a split pass: every attachment stores. And the description that goes on after a split:
/// every attachment loads. None when an attachment is memoryless, whose content a split would lose.
pub(super) fn descs(desc: &RenderPassDesc) -> Option<(RenderPassDesc, RenderPassDesc)> {
    let memoryless = desc.colors.iter().flatten().any(|c| c.memoryless)
        || desc.depth.as_ref().is_some_and(|d| d.memoryless)
        || desc.stencil.as_ref().is_some_and(|s| s.memoryless);
    if memoryless {
        return None;
    }
    let mut first = desc.clone();
    for c in first.colors.iter_mut().flatten() {
        c.store = StoreAction::Store;
    }
    if let Some(d) = first.depth.as_mut() {
        d.store = StoreAction::Store;
    }
    if let Some(s) = first.stencil.as_mut() {
        s.store = StoreAction::Store;
    }
    let mut again = first.clone();
    for c in again.colors.iter_mut().flatten() {
        c.load = LoadAction::Load;
    }
    if let Some(d) = again.depth.as_mut() {
        d.load = LoadAction::Load;
    }
    if let Some(s) = again.stencil.as_mut() {
        s.load = LoadAction::Load;
    }
    Some((first, again))
}

/// The barrier at the start of a pass of a split: it waits for all earlier work.
pub(super) fn barrier() -> Dependency {
    Dependency {
        scope: Scope::FromEarlierPasses,
        after: Stages::ALL,
        before: Stages::ALL,
        visibility: Visibility::Device,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_metal::{ColorAttachment, Context, DepthAttachment, Format, Texture, TextureDesc};

    #[test]
    fn a_split_pass_stores_everything_and_goes_on_loading() {
        let Ok(ctx) = Context::system_default() else {
            return;
        };
        let tex = |f| Texture::new(&ctx, TextureDesc::render_target_2d(8, 8, f)).unwrap();
        let desc = RenderPassDesc {
            colors: vec![
                Some(ColorAttachment {
                    texture: tex(Format::Rgba8Unorm),
                    depth_plane: 0,
                    load: LoadAction::Clear,
                    store: StoreAction::DontCare,
                    clear: [0.0; 4],
                    memoryless: false,
                }),
                None,
            ],
            depth: Some(DepthAttachment {
                texture: tex(Format::Depth32Float),
                load: LoadAction::DontCare,
                store: StoreAction::DontCare,
                clear: 1.0,
                memoryless: false,
            }),
            stencil: None,
        };
        let (first, again) = descs(&desc).expect("splittable");
        let c = first.colors[0].as_ref().unwrap();
        assert_eq!((c.load, c.store), (LoadAction::Clear, StoreAction::Store));
        let d = first.depth.as_ref().unwrap();
        assert_eq!(
            (d.load, d.store),
            (LoadAction::DontCare, StoreAction::Store)
        );
        let c = again.colors[0].as_ref().unwrap();
        assert_eq!((c.load, c.store), (LoadAction::Load, StoreAction::Store));
        assert_eq!(again.depth.as_ref().unwrap().load, LoadAction::Load);
        assert!(again.colors[1].is_none());
        let mut lost = desc.clone();
        lost.depth.as_mut().unwrap().memoryless = true;
        assert!(
            descs(&lost).is_none(),
            "a memoryless attachment cannot be split"
        );
    }
}
