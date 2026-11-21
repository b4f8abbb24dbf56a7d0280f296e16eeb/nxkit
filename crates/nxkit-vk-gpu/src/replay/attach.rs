//! The Metal attachments of a render segment from its load and store actions.

use nxkit_metal::{
    ColorAttachment, DepthAttachment, LoadAction, RenderPassDesc, StencilAttachment, StoreAction,
    Texture,
};
use nxkit_vk::format::ClearColor;

use crate::passplan::{Actions, Load, Store};

fn store(s: Store) -> StoreAction {
    match s {
        Store::Store => StoreAction::Store,
        Store::DontCare => StoreAction::DontCare,
    }
}

fn load(l: Load) -> LoadAction {
    match l {
        Load::Load => LoadAction::Load,
        Load::Clear(_) => LoadAction::Clear,
    }
}

fn raw(l: Load) -> [u32; 4] {
    match l {
        Load::Clear(v) => v,
        Load::Load => [0; 4],
    }
}

/// The Metal attachments of a segment.
pub(super) fn desc(
    colors: &[Option<Texture>],
    depth: Option<&Texture>,
    formats: &[Option<u32>],
    a: &Actions,
) -> RenderPassDesc {
    let colors = colors
        .iter()
        .zip(&a.colors)
        .enumerate()
        .map(|(i, (t, act))| {
            let (t, (l, s)) = (t.as_ref()?, (*act)?);
            let clear = match formats
                .get(i)
                .copied()
                .flatten()
                .and_then(nxkit_vk::format::lookup)
            {
                Some(f) => match f.clear_color(raw(l)) {
                    ClearColor::Float(c) => c.map(f64::from),
                    ClearColor::UInt(c) => c.map(f64::from),
                    ClearColor::SInt(c) => c.map(f64::from),
                },
                None => [0.0; 4],
            };
            Some(ColorAttachment {
                texture: t.clone(),
                depth_plane: 0,
                load: load(l),
                store: store(s),
                clear,
                memoryless: false,
            })
        })
        .collect();
    RenderPassDesc {
        colors,
        depth: depth.zip(a.depth).map(|(t, (l, s))| DepthAttachment {
            texture: t.clone(),
            load: load(l),
            store: store(s),
            clear: f64::from(f32::from_bits(raw(l)[0])),
            memoryless: false,
        }),
        stencil: depth.zip(a.stencil).map(|(t, (l, s))| StencilAttachment {
            texture: t.clone(),
            load: load(l),
            store: store(s),
            clear: raw(l)[1],
            memoryless: false,
        }),
    }
}
