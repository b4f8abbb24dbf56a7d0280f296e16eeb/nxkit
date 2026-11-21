//! The vertex input of a pipeline in Metal terms (the vertex part of the reference layer's `pipe_make`).
//!
//! The descriptors and the push constants of the vertex shader have the low buffer slots (the build gave them in
//! (set, binding) order). Each vertex buffer binding that an attribute reads takes the highest free buffer slot, from
//! 30 down. A binding with stride 0 gives one value to every vertex: Metal's constant step function.

use nxkit_abi::vk_table::{BUFFER_SLOTS, Resource};
use nxkit_vk::desc::{VertexAttribute, VertexBinding};
use nxkit_vk::format::VertexFormat;

/// How a vertex buffer steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Step {
    /// One element per vertex.
    Vertex,
    /// One element per instance.
    Instance,
    /// One element for everything (stride 0).
    Constant,
}

/// One vertex buffer of the pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Layout {
    /// The Vulkan binding.
    pub(crate) binding: u32,
    /// The Metal buffer slot.
    pub(crate) slot: u32,
    /// The stride in bytes (16 for a constant binding: Metal wants one, and ignores it).
    pub(crate) stride: u32,
    /// How it steps.
    pub(crate) step: Step,
}

/// One attribute of the pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Attribute {
    /// The shader location.
    pub(crate) location: u32,
    /// The format.
    pub(crate) format: VertexFormat,
    /// The offset in the element.
    pub(crate) offset: u32,
    /// The Metal buffer slot of its binding.
    pub(crate) slot: u32,
}

/// The vertex input of a pipeline.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct VertexPlan {
    /// The vertex buffers, in binding order.
    pub(crate) layouts: Vec<Layout>,
    /// The attributes, in the game's order.
    pub(crate) attributes: Vec<Attribute>,
    /// The attributes and bindings that were left out, for the log.
    pub(crate) skipped: Vec<String>,
}

impl VertexPlan {
    /// The Metal slot of a Vulkan binding.
    pub(crate) fn slot(&self, binding: u32) -> Option<u32> {
        self.layouts
            .iter()
            .find(|l| l.binding == binding)
            .map(|l| l.slot)
    }
}

/// The buffer slots that the vertex shader's descriptors and push constants take.
fn used_slots(resources: &[Resource]) -> u32 {
    let mut used = 0u32;
    for r in resources.iter().filter(|r| r.kind.is_buffer()) {
        for k in 0..r.count {
            if let Some(bit) = 1u32.checked_shl(r.slot + k) {
                used |= bit;
            }
        }
    }
    used
}

/// Plans the vertex input. An attribute with a format that is no vertex format, at a location above 30 or on a
/// binding above 13 is left out and named; a pipeline whose vertex buffers find no free slot is an error.
pub(crate) fn plan(
    resources: &[Resource],
    bindings: &[VertexBinding],
    attributes: &[VertexAttribute],
) -> Result<VertexPlan, String> {
    let mut p = VertexPlan::default();
    let mut read = 0u32;
    let mut kept = Vec::new();
    for a in attributes {
        match a.vertex {
            Some(format) if a.location < 31 && a.binding < 14 => {
                read |= 1 << a.binding;
                kept.push((a, format));
            }
            _ => p.skipped.push(format!(
                "vertex attribute {} (binding {}, format {})",
                a.location, a.binding, a.format
            )),
        }
    }
    let mut used = used_slots(resources);
    let mut sorted: Vec<&VertexBinding> = bindings.iter().collect();
    sorted.sort_by_key(|b| b.binding);
    for b in sorted {
        if b.binding >= 14 {
            p.skipped
                .push(format!("vertex binding {} over 13", b.binding));
            continue;
        }
        if read & (1 << b.binding) == 0 {
            continue;
        }
        let Some(slot) = (0..BUFFER_SLOTS).rev().find(|s| used & (1 << s) == 0) else {
            return Err(format!(
                "no free Metal buffer slot for vertex binding {}",
                b.binding
            ));
        };
        used |= 1 << slot;
        let (stride, step) = match (b.stride, b.per_instance) {
            (0, _) => (16, Step::Constant),
            (s, true) => (s, Step::Instance),
            (s, false) => (s, Step::Vertex),
        };
        p.layouts.push(Layout {
            binding: b.binding,
            slot,
            stride,
            step,
        });
    }
    for (a, format) in kept {
        match p.slot(a.binding) {
            Some(slot) => p.attributes.push(Attribute {
                location: a.location,
                format,
                offset: a.offset,
                slot,
            }),
            None => p.skipped.push(format!(
                "vertex attribute {} reads binding {}, which the pipeline does not describe",
                a.location, a.binding
            )),
        }
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use nxkit_abi::vk_table::Kind;

    use super::*;

    fn binding(binding: u32, stride: u32, per_instance: bool) -> VertexBinding {
        VertexBinding {
            binding,
            stride,
            per_instance,
        }
    }

    fn attr(location: u32, binding: u32, vertex: Option<VertexFormat>) -> VertexAttribute {
        VertexAttribute {
            location,
            binding,
            format: 106,
            vertex,
            offset: location * 4,
        }
    }

    #[test]
    fn bindings_take_the_highest_free_slots() {
        let resources = [
            Resource {
                kind: Kind::Uniform,
                set: 0,
                binding: 0,
                slot: 29,
                count: 2,
            },
            Resource {
                kind: Kind::Texture,
                set: 0,
                binding: 1,
                slot: 30,
                count: 1,
            },
        ];
        let f = Some(VertexFormat::Float3);
        let p = plan(
            &resources,
            &[
                binding(1, 0, false),
                binding(0, 32, false),
                binding(2, 12, true),
            ],
            &[
                attr(0, 0, f),
                attr(1, 1, f),
                attr(2, 2, f),
                attr(3, 0, None),
            ],
        );
        let p = p.unwrap_or_default();
        // Slots 29 and 30 hold the uniform array (a texture slot does not count).
        assert_eq!(
            p.layouts,
            vec![
                Layout {
                    binding: 0,
                    slot: 28,
                    stride: 32,
                    step: Step::Vertex
                },
                Layout {
                    binding: 1,
                    slot: 27,
                    stride: 16,
                    step: Step::Constant
                },
                Layout {
                    binding: 2,
                    slot: 26,
                    stride: 12,
                    step: Step::Instance
                },
            ]
        );
        assert_eq!(
            p.attributes.iter().map(|a| a.slot).collect::<Vec<_>>(),
            vec![28, 27, 26]
        );
        assert_eq!(p.skipped.len(), 1);
        assert_eq!(p.slot(2), Some(26));
    }

    #[test]
    fn a_binding_no_attribute_reads_takes_no_slot() {
        let p = plan(
            &[],
            &[binding(0, 16, false), binding(3, 8, false)],
            &[attr(0, 3, Some(VertexFormat::Float2))],
        );
        let p = p.unwrap_or_default();
        assert_eq!(p.layouts.len(), 1);
        assert_eq!((p.layouts[0].binding, p.layouts[0].slot), (3, 30));
    }

    #[test]
    fn a_full_slot_range_refuses_the_pipeline() {
        let resources = [Resource {
            kind: Kind::Storage,
            set: 0,
            binding: 0,
            slot: 0,
            count: 31,
        }];
        let r = plan(
            &resources,
            &[binding(0, 16, false)],
            &[attr(0, 0, Some(VertexFormat::Float))],
        );
        assert!(r.is_err());
    }
}
