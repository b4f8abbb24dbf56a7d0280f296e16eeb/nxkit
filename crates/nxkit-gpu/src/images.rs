//! Storage images (`img` bindings): which textures the shaders of a frame read and write as images, and the views they
//! get.
//!
//! The game binds image handles with `BindImage` (the walker puts them into `DrawDetail::images` and
//! `DispatchDetail::images`, in the slots of the texture banks). A translated shader declares each `img` member with
//! its texture type, its access and, for image atomics, an `atomic` flag. This module pairs the two:
//!
//! - [`uses`] runs before the textures of the frame are made. It finds the texture of each bound image, so that a
//!   texture with image atomics is made with `shader_atomic` (a 32 bit colour format is then stored as `R32Uint`, see
//!   `nxkit_metal::TextureDesc`), and so that a render pass knows which textures its fragment shaders write. A
//!   dispatch needs no such list: the front end already puts every bound image into the `writes` of the dispatch.
//! - [`resolve`] makes the view of each (handle, declared type, atomic) and gives its resource ID to the argument
//!   buffers (`Env::images`).

use nxkit_metal::TextureKind;
use nxkit_nvn::frame::{Cmd, Frame};
use nxkit_nvn::passes::{PassKind, PassPlan};
use nxkit_nvn::resource::TexId;

use crate::bindless::{Handles, ImageRes};
use crate::detail::{FrameDetail, Stage, TextureBind};
use crate::draws::{FastMap, FastSet, Ordinal};
use crate::programs::{BindKind, ImgDecl, Programs};
use crate::snapshot::EncodeView;
use crate::stats::{EncodeStats, Skip};
use crate::textures::{Res, TextureStore};

/// An `img` member of a shader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Decl {
    /// The image slot (`reg`).
    pub(crate) reg: u32,
    /// The texture type that the shader declares.
    pub(crate) dim: TextureKind,
    /// The access and the atomic flag.
    pub(crate) img: ImgDecl,
}

/// The `img` members of the shaders by key (index 1 for vertex, as in `Programs`).
pub(crate) type Decls = [FastMap<String, Vec<Decl>>; 2];

/// The `img` members of the program of `key`, cached by key.
pub(crate) fn declared<'a>(
    decls: &'a mut Decls,
    programs: &Programs,
    key: &str,
    vertex: bool,
) -> &'a [Decl] {
    let map = &mut decls[usize::from(vertex)];
    if !map.contains_key(key) {
        let v = programs
            .stage(key, vertex)
            .map(|p| {
                p.binds
                    .iter()
                    .filter(|b| b.kind == BindKind::Img)
                    .filter_map(|b| {
                        Some(Decl {
                            reg: b.reg,
                            dim: b.dim?,
                            img: b.img?,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        map.insert(key.to_owned(), v);
    }
    map.get(key).map_or(&[][..], Vec::as_slice)
}

/// The image binds of one stage of one draw or dispatch.
pub(crate) struct Site<'a> {
    pub(crate) key: &'a str,
    pub(crate) vertex: bool,
    pub(crate) stage: Stage,
    pub(crate) binds: &'a [TextureBind],
    /// The ordinal of the draw, `None` for a dispatch.
    pub(crate) draw: Option<u32>,
}

/// Every stage of the frame that has image binds.
pub(crate) fn sites(detail: &FrameDetail) -> Vec<Site<'_>> {
    let mut out = Vec::new();
    for (n, d) in detail.draws.iter().enumerate() {
        if d.images.is_empty() {
            continue;
        }
        let stages = [
            (
                Some(&*d.program.vertex).filter(|k| !k.is_empty()),
                true,
                Stage::Vertex,
            ),
            (d.program.fragment.as_deref(), false, Stage::Fragment),
            (d.program.task.as_deref(), false, Stage::Task),
            (d.program.mesh.as_deref(), false, Stage::Mesh),
        ];
        for (key, vertex, stage) in stages {
            if let Some(key) = key {
                out.push(Site {
                    key,
                    vertex,
                    stage,
                    binds: &d.images,
                    draw: Some(n as u32),
                });
            }
        }
    }
    for d in detail
        .dispatches
        .iter()
        .filter(|d| !d.images.is_empty() && !d.program.is_empty())
    {
        out.push(Site {
            key: &d.program,
            vertex: false,
            stage: Stage::Compute,
            binds: &d.images,
            draw: None,
        });
    }
    out
}

/// The textures a render pass reads and writes through images, beyond what its commands list.
#[derive(Clone, Debug, Default)]
pub(crate) struct PassImages {
    /// Images that are only read.
    pub(crate) reads: Vec<TexId>,
    /// Images that are written (also the read-write ones).
    pub(crate) writes: Vec<TexId>,
}

/// What [`uses`] found.
#[derive(Default)]
pub(crate) struct ImageUses {
    /// The textures of images on which a shader runs atomics.
    pub(crate) atomic: FastSet<TexId>,
    /// The textures of the images of draws and dispatches.
    pub(crate) all: FastSet<TexId>,
    /// For each draw, the textures it reads and writes through images.
    pub(crate) per_draw: FastMap<u32, PassImages>,
}

/// Pairs the image binds of the frame with the `img` members of their shaders.
pub(crate) fn uses(
    decls: &mut Decls,
    programs: &Programs,
    view: &dyn EncodeView,
    detail: &FrameDetail,
) -> ImageUses {
    let mut out = ImageUses::default();
    for site in sites(detail) {
        for d in declared(decls, programs, site.key, site.vertex) {
            let Some(h) = site
                .binds
                .iter()
                .find(|t| t.stage == site.stage && t.slot == d.reg)
                .map(|t| t.handle)
            else {
                continue;
            };
            let Some(id) = (h != 0)
                .then(|| view.resolve(h).ok())
                .flatten()
                .map(|(s, _)| s.texture)
            else {
                continue;
            };
            out.all.insert(id);
            if d.img.atomic {
                out.atomic.insert(id);
            }
            if let Some(n) = site.draw {
                let e = out.per_draw.entry(n).or_default();
                let list = if d.img.writes() {
                    &mut e.writes
                } else {
                    &mut e.reads
                };
                if !list.contains(&id) {
                    list.push(id);
                }
            }
        }
    }
    out
}

/// For each pass, the textures its draws read and write through images. Only render passes have any.
pub(crate) fn pass_images(
    frame: &Frame,
    plan: &PassPlan,
    ords: &[Ordinal],
    per_draw: &FastMap<u32, PassImages>,
) -> Vec<PassImages> {
    plan.passes
        .iter()
        .map(|p| {
            let mut out = PassImages::default();
            if p.kind != PassKind::Render || per_draw.is_empty() {
                return out;
            }
            for i in p.cmds.clone() {
                if !matches!(frame.cmds.get(i as usize), Some(Cmd::Draw { .. })) {
                    continue;
                }
                let Some(u) = ords.get(i as usize).and_then(|o| per_draw.get(&o.draw)) else {
                    continue;
                };
                for (from, to) in [(&u.reads, &mut out.reads), (&u.writes, &mut out.writes)] {
                    for id in from {
                        if !to.contains(id) {
                            to.push(*id);
                        }
                    }
                }
            }
            out
        })
        .collect()
}

/// Makes the view of every image the frame binds. A view that cannot be made (nothing bound, a texture a shader cannot
/// write, atomics on a texture that was not made for them) is entered with the resource ID 0, and the binding then
/// reads and writes nothing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve(
    store: &mut TextureStore,
    decls: &mut Decls,
    programs: &Programs,
    view: &dyn EncodeView,
    detail: &FrameDetail,
    scale: f32,
    textures: &mut FastMap<TexId, Res>,
    handles: &mut Handles,
    stats: &mut EncodeStats,
) {
    for site in sites(detail) {
        for d in declared(decls, programs, site.key, site.vertex) {
            let Some(h) = site
                .binds
                .iter()
                .find(|t| t.stage == site.stage && t.slot == d.reg)
                .map(|t| t.handle)
            else {
                continue;
            };
            let key = (h, d.dim, d.img.atomic_code());
            if h == 0 || handles.images.contains_key(&key) {
                continue;
            }
            let r = one(store, view, h, d, scale, textures, handles, stats).unwrap_or_else(|why| {
                stats.skip(why);
                ImageRes::default()
            });
            handles.images.insert(key, r);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn one(
    store: &mut TextureStore,
    view: &dyn EncodeView,
    handle: u64,
    d: &Decl,
    scale: f32,
    textures: &mut FastMap<TexId, Res>,
    handles: &mut Handles,
    stats: &mut EncodeStats,
) -> Result<ImageRes, Skip> {
    let (slot, _) = view.resolve(handle).map_err(|e| {
        let what = format_args!("image handle {handle:#x}: {e:?}");
        nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::HandleUnresolved, what);
        crate::tex_skip(what)
    })?;
    let id = slot.texture;
    if !textures.contains_key(&id) {
        let rec = view
            .record(id)
            .map_err(|e| crate::tex_skip(format_args!("image {id:?}: {e:?}")))?;
        let res = store.ensure(rec, scale, crate::textures::Use::default(), stats)?;
        textures.insert(id, res);
    }
    let res = textures.get(&id).ok_or(Skip::Texture)?;
    if !res.desc.shader_write && d.img.writes() {
        return Err(crate::tex_skip(format_args!(
            "image {id:?} {:?} cannot be written by a shader",
            res.desc.format
        )));
    }
    let tex = store.image_view(id, res, &slot.header, d.dim, d.img.atomic_code(), stats)?;
    let r = ImageRes {
        id: tex.gpu_resource_id(),
        scaled: res.scaled,
    };
    handles.keep.push(tex);
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detail::{DispatchDetail, FrameDetail};
    use crate::programs::ImgAccess;

    fn bind(stage: Stage, slot: u32, handle: u64) -> TextureBind {
        TextureBind {
            stage,
            slot,
            handle,
        }
    }

    #[test]
    fn sites_pair_each_stage_with_its_key() {
        let mut detail = FrameDetail::default();
        detail.dispatches.push(DispatchDetail {
            program: "cs".into(),
            push: None,
            groups: [1; 3],
            ubos: vec![],
            sbufs: vec![],
            textures: vec![],
            images: vec![bind(Stage::Compute, 1, 9)],
        });
        detail.dispatches.push(DispatchDetail {
            program: "".into(),
            push: None,
            groups: [1; 3],
            ubos: vec![],
            sbufs: vec![],
            textures: vec![],
            images: vec![bind(Stage::Compute, 0, 3)],
        });
        let s = sites(&detail);
        assert_eq!(s.len(), 1);
        assert_eq!(
            (s[0].key, s[0].stage, s[0].draw),
            ("cs", Stage::Compute, None)
        );
        assert_eq!(s[0].binds[0].handle, 9);
    }

    #[test]
    fn an_atomic_or_written_image_counts_as_a_write() {
        let read = ImgDecl {
            access: ImgAccess::Read,
            atomic: false,
            signed: false,
        };
        let rw = ImgDecl {
            access: ImgAccess::ReadWrite,
            atomic: false,
            signed: false,
        };
        let atomic = ImgDecl {
            access: ImgAccess::Read,
            atomic: true,
            signed: true,
        };
        assert!(!read.writes());
        assert!(rw.writes());
        assert!(atomic.writes());
        assert_eq!((read.atomic_code(), atomic.atomic_code()), (0, 2));
    }
}
