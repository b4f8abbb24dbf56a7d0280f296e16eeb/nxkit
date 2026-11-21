//! Descriptor reflection and the Metal slot of each resource, as `vk_shaders.py` numbers them.
//!
//! The resources are numbered in (set, binding) order per Metal class: buffers for uniform and storage buffers and the
//! push-constant block, textures for images and combined image samplers, samplers for samplers and combined image samplers.
//! The sampler of a split combined image sampler has the binding with `crate::spirv::TWIN_BINDING` set and counts as part
//! of its image's binding.
//! An array of descriptors is an error for now: naga's MSL writer passes it in an argument buffer, and the host's table
//! has no kind for one (the old engine's table gave it `k` consecutive slots, `*k`). A combined image sampler takes texture slot `n` and sampler slot
//! `n`, the first `n` that is free in both classes. `vk_shaders.py` took the next texture slot, which could be a sampler
//! slot that a separate sampler before it already had. The push-constant block gets the next buffer slot after the
//! descriptor buffers.

use std::collections::{BTreeMap, BTreeSet};

use naga::back::msl::{BindSamplerTarget, BindTarget, EntryPointResources};
use naga::{
    AddressSpace, GlobalVariable, Handle, Module, ResourceBinding, TypeInner, valid::ModuleInfo,
};

use crate::spirv::TWIN_BINDING;
use crate::{Kind, Resource};

/// The highest slot count per class: 16 buffers (the vertex buffers take the slots above), 31 textures and 16 samplers.
pub(crate) const LIMITS: (u32, u32, u32) = (16, 31, 16);

/// One descriptor binding of the shader, its Metal slot and its globals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Res {
    /// The binding and its slot.
    pub res: Resource,
    /// The globals of this binding: one, or the image and the sampler of a combined image sampler.
    pub globals: Vec<Handle<GlobalVariable>>,
}

/// The resources and the slot assignment of a module.
#[derive(Clone, Debug, Default)]
pub(crate) struct Slots {
    pub res: Vec<Res>,
    pub push: Option<Res>,
}

fn kind_of(
    m: &Module,
    g: &GlobalVariable,
    combined: &BTreeSet<(u32, u32)>,
    rb: &ResourceBinding,
) -> Result<Option<Kind>, String> {
    let is_combined = combined.contains(&(rb.group, rb.binding & !TWIN_BINDING));
    Ok(Some(match (g.space, &m.types[g.ty].inner) {
        // naga's MSL writer passes an array of textures or samplers in an argument buffer, not at consecutive slots
        (_, TypeInner::BindingArray { .. }) => {
            return Err(format!(
                "set {} binding {} is an array of descriptors",
                rb.group, rb.binding
            ));
        }
        (AddressSpace::Uniform, _) => Kind::Uniform,
        (AddressSpace::Storage { .. }, _) => Kind::Storage,
        (AddressSpace::Handle, TypeInner::Image { .. } | TypeInner::Sampler { .. })
            if is_combined =>
        {
            Kind::Combined
        }
        (AddressSpace::Handle, TypeInner::Image { .. }) => Kind::Texture,
        (AddressSpace::Handle, TypeInner::Sampler { .. }) => Kind::Sampler,
        _ => return Ok(None),
    }))
}

/// Numbers the module's resources. `combined` lists the (set, binding) pairs that were combined image samplers before
/// `crate::spirv::split_combined`. An error names a class whose slots run over the limits.
pub(crate) fn assign(m: &Module, combined: &BTreeSet<(u32, u32)>) -> Result<Slots, String> {
    let mut by_pair: BTreeMap<(u32, u32), Res> = BTreeMap::new();
    let mut push = None;
    for (h, g) in m.global_variables.iter() {
        if g.space == AddressSpace::Immediate {
            push = Some(Res {
                res: Resource {
                    kind: Kind::Push,
                    set: 0,
                    binding: 0,
                    slot: 0,
                    count: 1,
                },
                globals: vec![h],
            });
            continue;
        }
        let Some(rb) = g.binding else { continue };
        let Some(kind) = kind_of(m, g, combined, &rb)? else {
            continue;
        };
        let binding = rb.binding & !TWIN_BINDING;
        let e = by_pair.entry((rb.group, binding)).or_insert(Res {
            res: Resource {
                kind,
                set: rb.group,
                binding,
                slot: 0,
                count: 1,
            },
            globals: Vec::new(),
        });
        if e.res.kind != kind {
            return Err(format!(
                "set {} binding {} holds both a {:?} and a {:?}",
                rb.group, binding, e.res.kind, kind
            ));
        }
        e.globals.push(h);
    }
    let (mut buf, mut tex, mut smp) = (0, 0, 0);
    let mut res: Vec<Res> = by_pair.into_values().collect();
    for r in &mut res {
        let r = &mut r.res;
        if r.kind == Kind::Combined {
            r.slot = tex.max(smp);
            tex = r.slot + r.count;
            smp = tex;
            continue;
        }
        let next = if r.kind.is_buffer() {
            &mut buf
        } else if r.kind.is_texture() {
            &mut tex
        } else {
            &mut smp
        };
        r.slot = *next;
        *next += r.count;
    }
    if let Some(p) = push.as_mut() {
        p.res.slot = buf;
        buf += 1;
    }
    if buf > LIMITS.0 || tex > LIMITS.1 || smp > LIMITS.2 {
        return Err(format!(
            "resource slots exceeded: buffer {buf}, texture {tex}, sampler {smp}"
        ));
    }
    Ok(Slots { res, push })
}

impl Slots {
    /// naga's binding map for one entry point.
    pub(crate) fn binding_map(&self) -> EntryPointResources {
        let mut ep = EntryPointResources::default();
        for Res { res: r, .. } in &self.res {
            let slot = r.slot as u8;
            let mut t = BindTarget {
                mutable: r.kind == Kind::Storage,
                ..BindTarget::default()
            };
            if r.kind.is_buffer() {
                t.buffer = Some(slot);
            }
            if r.kind.is_texture() {
                t.texture = Some(slot);
            }
            let sampler = Some(BindSamplerTarget::Resource(slot));
            let at = |binding| ResourceBinding {
                group: r.set,
                binding,
            };
            match r.kind {
                Kind::Sampler => t.sampler = sampler,
                Kind::Combined => {
                    let s = BindTarget {
                        sampler,
                        ..BindTarget::default()
                    };
                    ep.resources.insert(at(r.binding | TWIN_BINDING), s);
                }
                _ => {}
            }
            ep.resources.insert(at(r.binding), t);
        }
        ep.immediates_buffer = self.push.as_ref().map(|p| p.res.slot as u8);
        ep
    }

    /// The resources that entry point `ep` uses, in (set, binding) order and the push constants last.
    pub(crate) fn used(&self, info: &ModuleInfo, ep: usize) -> Vec<Resource> {
        let fi = info.get_entry_point(ep);
        self.res
            .iter()
            .chain(self.push.iter())
            .filter(|r| r.globals.iter().any(|&g| !fi[g].is_empty()))
            .map(|r| r.res)
            .collect()
    }
}
