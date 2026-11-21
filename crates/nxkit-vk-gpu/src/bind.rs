//! Which descriptor goes to which Metal slot (the reference layer's `C_SETS` and `bind_res`).
//!
//! A shader's row lists its resources as `kind:set.binding=slot[*count]`. A draw binds, for each one, the descriptors
//! of the set that is bound at `set`: element `k` of the binding goes to slot `slot + k`, a dynamic buffer gets its
//! dynamic offset added, and a combined image sampler takes the texture slot and the sampler slot of the same number.
//! Everything here is pure: the sets come in, the slots go out.

use nxkit_abi::vk_table::{Kind, Resource};
use nxkit_vk::backend::SetContents;
use nxkit_vk::desc::{BufferRef, Descriptor, SamplerId, TextureId};

/// The descriptor sets of a bind point.
pub(crate) const SETS: usize = 8;

/// A set as it is bound: its handle and its dynamic offsets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BoundSet {
    /// The handle of the set.
    pub(crate) handle: u64,
    /// The dynamic offsets of its dynamic bindings, in the order of the layout.
    pub(crate) dynamic: Vec<u32>,
}

/// `vkCmdBindDescriptorSets` on the sets of one bind point. `dynamic_count` gives the dynamic offsets that a set
/// takes, from its layout; a set that the submission lacks (the game freed it) takes none and binds nothing.
pub(crate) fn bind_sets(
    bound: &mut [Option<BoundSet>; SETS],
    first: u32,
    handles: &[u64],
    offsets: &[u32],
    dynamic_count: impl Fn(u64) -> Option<u32>,
) {
    let mut taken = 0usize;
    for (i, &h) in handles.iter().enumerate() {
        let n = dynamic_count(h).unwrap_or(0) as usize;
        let end = (taken + n).min(offsets.len());
        let dynamic = offsets.get(taken..end).unwrap_or_default().to_vec();
        taken = end;
        if let Some(slot) = bound.get_mut(first as usize + i) {
            *slot = Some(BoundSet { handle: h, dynamic });
        }
    }
}

/// One Metal binding of a draw or dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Bound {
    /// A buffer at a buffer slot: the memory and the offset of the first byte (with its dynamic offset).
    Buffer {
        /// The slot.
        slot: u32,
        /// The range.
        at: BufferRef,
    },
    /// A texture at a texture slot.
    Texture {
        /// The slot.
        slot: u32,
        /// The view or texel buffer.
        id: TextureId,
    },
    /// A sampler at a sampler slot.
    Sampler {
        /// The slot.
        slot: u32,
        /// The sampler.
        id: SamplerId,
    },
}

/// Resolves the resources of one shader against the bound sets. `contents` finds a bound set's contents in the
/// submission. What cannot be bound is named in the problems, once per resource, for the log.
pub(crate) fn resolve<'a>(
    resources: &[Resource],
    bound: &[Option<BoundSet>; SETS],
    contents: impl Fn(u64) -> Option<&'a SetContents>,
    out: &mut Vec<Bound>,
    problems: &mut Vec<String>,
) {
    for r in resources.iter().filter(|r| r.kind != Kind::Push) {
        let Some(set) = bound.get(r.set as usize).and_then(Option::as_ref) else {
            problems.push(format!("set {} is not bound", r.set));
            continue;
        };
        let Some(c) = contents(set.handle) else {
            problems.push(format!("set {} was freed", r.set));
            continue;
        };
        let Some(lb) = c
            .layout
            .index_of(r.binding)
            .and_then(|i| c.layout.bindings.get(i))
        else {
            problems.push(format!(
                "{}.{} is not in the layout of the bound set",
                r.set, r.binding
            ));
            continue;
        };
        for k in 0..r.count.min(lb.count) {
            let slot = r.slot + k;
            let d = c
                .descriptors
                .get((lb.base + k) as usize)
                .copied()
                .unwrap_or_default();
            let immutable = lb.immutable.get(k as usize).copied();
            let empty = || format!("{} {}.{}[{k}] is empty", r.kind.letter(), r.set, r.binding);
            match (r.kind, d) {
                (Kind::Uniform | Kind::Storage, Descriptor::Buffer(mut at)) => {
                    let dynamic = lb
                        .dynamic
                        .and_then(|first| set.dynamic.get((first + k) as usize));
                    if let Some(&o) = dynamic {
                        at.offset += u64::from(o);
                        at.size = at.size.saturating_sub(u64::from(o));
                    }
                    out.push(Bound::Buffer { slot, at });
                }
                (Kind::Texture, Descriptor::Image { view, .. } | Descriptor::TexelBuffer(view)) => {
                    out.push(Bound::Texture { slot, id: view });
                }
                (Kind::Combined, Descriptor::Image { view, sampler }) => {
                    out.push(Bound::Texture { slot, id: view });
                    match sampler.or(immutable) {
                        Some(id) => out.push(Bound::Sampler { slot, id }),
                        None => problems.push(format!("the sampler of {}", empty())),
                    }
                }
                (Kind::Sampler, Descriptor::Sampler(id)) => out.push(Bound::Sampler { slot, id }),
                (Kind::Sampler, Descriptor::Empty) if immutable.is_some() => {
                    if let Some(id) = immutable {
                        out.push(Bound::Sampler { slot, id });
                    }
                }
                (_, Descriptor::Empty) => problems.push(empty()),
                (kind, d) => problems.push(format!(
                    "{}.{}[{k}] is a {kind:?} in the shader and {d:?} in the set",
                    r.set, r.binding
                )),
            }
        }
    }
}

/// The push-constant slot of a shader, if it takes the block.
pub(crate) fn push_slot(resources: &[Resource]) -> Option<u32> {
    resources
        .iter()
        .find(|r| r.kind == Kind::Push)
        .map(|r| r.slot)
}

/// The bytes of the push-constant block that a draw binds: the whole range of the pipeline layout, at least 16
/// bytes, a multiple of 16 and at most the 256 that the stage keeps.
pub(crate) fn push_len(push_size: u32) -> usize {
    (push_size.max(16).next_multiple_of(16)).min(256) as usize
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nxkit_vk::desc::{LayoutBinding, MemoryId, SetLayoutDesc};

    use super::*;

    fn res(kind: Kind, set: u32, binding: u32, slot: u32, count: u32) -> Resource {
        Resource {
            kind,
            set,
            binding,
            slot,
            count,
        }
    }

    fn buf(offset: u64) -> Descriptor {
        Descriptor::Buffer(BufferRef {
            memory: MemoryId(7),
            offset,
            size: 256,
        })
    }

    /// A set with a dynamic uniform buffer at binding 0, an array of two images at 2 and a sampler with an immutable
    /// sampler at 3.
    fn set() -> SetContents {
        let lb = |binding, ty, count, base, dynamic: Option<u32>, immutable: Vec<SamplerId>| {
            LayoutBinding {
                binding,
                ty,
                count,
                stages: 0x11,
                base,
                dynamic,
                immutable,
            }
        };
        SetContents {
            layout: Arc::new(SetLayoutDesc {
                bindings: vec![
                    lb(0, 8, 1, 0, Some(0), vec![]),
                    lb(2, 1, 2, 1, None, vec![]),
                    lb(3, 0, 1, 3, None, vec![SamplerId(99)]),
                ],
                total: 4,
                dynamic_count: 1,
            }),
            descriptors: Arc::new(vec![
                buf(512),
                Descriptor::Image {
                    view: TextureId(10),
                    sampler: Some(SamplerId(20)),
                },
                Descriptor::Image {
                    view: TextureId(11),
                    sampler: None,
                },
                Descriptor::Empty,
            ]),
        }
    }

    #[test]
    fn sets_take_their_dynamic_offsets_in_order() {
        let mut bound: [Option<BoundSet>; SETS] = Default::default();
        let counts = |h: u64| Some(if h == 1 { 2 } else { 1 });
        bind_sets(&mut bound, 1, &[1, 2], &[10, 20, 30], counts);
        assert_eq!(bound[0], None);
        assert_eq!(
            bound[1].as_ref().map(|b| b.dynamic.clone()),
            Some(vec![10, 20])
        );
        assert_eq!(bound[2].as_ref().map(|b| b.dynamic.clone()), Some(vec![30]));
        // A freed set takes no offsets and the next one still gets its own.
        bind_sets(&mut bound, 0, &[5, 2], &[40], |h| (h != 5).then_some(1));
        assert_eq!(
            bound[1].as_ref().map(|b| (b.handle, b.dynamic.clone())),
            Some((2, vec![40]))
        );
    }

    #[test]
    fn elements_go_to_consecutive_slots_with_dynamic_offsets() {
        let s = set();
        let mut bound: [Option<BoundSet>; SETS] = Default::default();
        bound[1] = Some(BoundSet {
            handle: 3,
            dynamic: vec![256],
        });
        let resources = [
            res(Kind::Uniform, 1, 0, 4, 1),
            res(Kind::Combined, 1, 2, 2, 2),
            res(Kind::Sampler, 1, 3, 7, 1),
            res(Kind::Push, 0, 0, 9, 1),
        ];
        let (mut out, mut problems) = (Vec::new(), Vec::new());
        resolve(
            &resources,
            &bound,
            |h| (h == 3).then_some(&s),
            &mut out,
            &mut problems,
        );
        assert_eq!(
            out,
            vec![
                Bound::Buffer {
                    slot: 4,
                    at: BufferRef {
                        memory: MemoryId(7),
                        offset: 768,
                        size: 0
                    }
                },
                Bound::Texture {
                    slot: 2,
                    id: TextureId(10)
                },
                Bound::Sampler {
                    slot: 2,
                    id: SamplerId(20)
                },
                Bound::Texture {
                    slot: 3,
                    id: TextureId(11)
                },
                Bound::Sampler {
                    slot: 7,
                    id: SamplerId(99)
                },
            ]
        );
        // The second combined element has no sampler.
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(push_slot(&resources), Some(9));
    }

    #[test]
    fn unbound_sets_and_missing_bindings_are_named() {
        let s = set();
        let mut bound: [Option<BoundSet>; SETS] = Default::default();
        bound[0] = Some(BoundSet {
            handle: 3,
            dynamic: vec![],
        });
        let resources = [
            res(Kind::Uniform, 2, 0, 0, 1),
            res(Kind::Texture, 0, 5, 0, 1),
        ];
        let (mut out, mut problems) = (Vec::new(), Vec::new());
        resolve(&resources, &bound, |_| Some(&s), &mut out, &mut problems);
        assert!(out.is_empty());
        assert_eq!(problems.len(), 2);
        assert!(problems[0].contains("set 2 is not bound"));
        assert!(problems[1].contains("0.5"));
    }

    #[test]
    fn push_blocks_are_whole_lines_of_16_bytes() {
        assert_eq!(push_len(0), 16);
        assert_eq!(push_len(20), 32);
        assert_eq!(push_len(256), 256);
        assert_eq!(push_len(4096), 256);
    }
}
