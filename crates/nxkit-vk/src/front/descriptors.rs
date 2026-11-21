//! Descriptor set layouts, pools, sets, their writes, copies and update templates, and pipeline layouts.

use std::collections::HashSet;
use std::sync::Arc;

use crate::consts::{descriptor_type as dt, special};
use crate::desc::{
    BufferRef, Descriptor, LayoutBinding, PipelineLayoutDesc, PushRange, SamplerId, SetLayoutDesc,
    TextureId,
};
use crate::info::{
    CopyInfo, SetLayoutBindingInfo, TemplateEntry, WriteInfo, WriteItem, template_writes,
};
use crate::mem::Mem;
use crate::result::{Log, VkError, VkResult};

use super::Vk;
use super::state::{DescriptorSet, State};

/// The most bindings in one set layout and the highest binding number (the reference layer's limits).
const MAX_BINDINGS: usize = 64;
const MAX_BINDING_NUMBER: u32 = 127;

fn is_dynamic(ty: u32) -> bool {
    ty == dt::UNIFORM_BUFFER_DYNAMIC || ty == dt::STORAGE_BUFFER_DYNAMIC
}

/// The descriptor that one write item makes, with the objects it names resolved. A binding with immutable samplers
/// keeps its own sampler.
fn descriptor(
    log: &Log,
    s: &State,
    b: &LayoutBinding,
    elem: u32,
    old: Descriptor,
    item: WriteItem,
) -> Descriptor {
    let imm = b.immutable.get(elem as usize).copied();
    let sampler = |h: u64| (h != 0 && s.samplers.get(h).is_some()).then_some(SamplerId(h));
    let view = |h: u64| {
        if s.views.get(h).is_some() {
            TextureId(h)
        } else {
            TextureId(0)
        }
    };
    match (b.ty, item) {
        (dt::SAMPLER, WriteItem::Image { sampler: h, .. }) => match imm {
            Some(i) => Descriptor::Sampler(i),
            None => sampler(h).map_or(Descriptor::Empty, Descriptor::Sampler),
        },
        (
            dt::COMBINED_IMAGE_SAMPLER,
            WriteItem::Image {
                sampler: h,
                view: v,
            },
        ) => Descriptor::Image {
            view: view(v),
            sampler: imm.or_else(|| sampler(h)),
        },
        (
            dt::SAMPLED_IMAGE | dt::STORAGE_IMAGE | dt::INPUT_ATTACHMENT,
            WriteItem::Image { view: v, .. },
        ) => Descriptor::Image {
            view: view(v),
            sampler: None,
        },
        (dt::UNIFORM_TEXEL_BUFFER | dt::STORAGE_TEXEL_BUFFER, WriteItem::TexelBuffer(v)) => {
            if s.buffer_views.get(v).is_some() {
                Descriptor::TexelBuffer(TextureId(v))
            } else {
                Descriptor::Empty
            }
        }
        (
            _,
            WriteItem::Buffer {
                buffer,
                offset,
                range,
            },
        ) => match s.buffers.get(buffer) {
            Some(buf) => {
                let r = buf.at(offset);
                Descriptor::Buffer(BufferRef {
                    size: if range == special::WHOLE_SIZE {
                        r.size
                    } else {
                        range
                    },
                    ..r
                })
            }
            None => Descriptor::Empty,
        },
        (ty, WriteItem::Unsupported(_)) => {
            log.once(
                "descriptor type",
                format_args!("[RED] descriptor type {ty} not implemented"),
            );
            old
        }
        (ty, _) => {
            log.once(
                "descriptor mismatch",
                format_args!("[RED] a write does not match descriptor type {ty}"),
            );
            old
        }
    }
}

impl Vk {
    /// `vkCreateDescriptorSetLayout`. The bindings are sorted by number: the order that dynamic offsets are taken in.
    pub fn create_set_layout(&self, bindings: &[SetLayoutBindingInfo]) -> VkResult<u64> {
        if bindings.len() > MAX_BINDINGS {
            self.log.once(
                "set layout size",
                format_args!(
                    "[RED] descriptor set layout with {} bindings, over {MAX_BINDINGS}",
                    bindings.len()
                ),
            );
            return Err(VkError::OutOfHostMemory);
        }
        let mut sorted: Vec<&SetLayoutBindingInfo> = bindings.iter().collect();
        sorted.sort_by_key(|b| b.binding);
        let mut d = SetLayoutDesc::default();
        let mut s = self.lock();
        for b in sorted {
            if b.binding > MAX_BINDING_NUMBER {
                self.log.once(
                    "binding number",
                    format_args!(
                        "[RED] binding number {} over {MAX_BINDING_NUMBER}",
                        b.binding
                    ),
                );
                continue;
            }
            let dynamic = is_dynamic(b.ty).then_some(d.dynamic_count);
            if dynamic.is_some() {
                d.dynamic_count += b.count;
            }
            let immutable = b
                .immutable
                .iter()
                .map(|&h| SamplerId(if s.samplers.get(h).is_some() { h } else { 0 }))
                .collect();
            d.bindings.push(LayoutBinding {
                binding: b.binding,
                ty: b.ty,
                count: b.count,
                stages: b.stages,
                base: d.total,
                dynamic,
                immutable,
            });
            d.total += b.count;
        }
        if d.dynamic_count > 16 {
            self.log.once(
                "dynamic offsets",
                format_args!(
                    "[RED] {} dynamic offsets in one set, over 16",
                    d.dynamic_count
                ),
            );
        }
        Ok(s.set_layouts.insert(Arc::new(d)))
    }

    /// `vkDestroyDescriptorSetLayout`. Sets and pipeline layouts made with it keep it.
    pub fn destroy_set_layout(&self, layout: u64) {
        self.lock().set_layouts.remove(layout);
    }

    /// `vkCreateDescriptorPool`. `maxSets` and the pool sizes are not enforced.
    pub fn create_descriptor_pool(&self) -> u64 {
        self.lock().pools.insert(HashSet::new())
    }

    /// `vkDestroyDescriptorPool`: its sets go with it.
    pub fn destroy_descriptor_pool(&self, pool: u64) {
        let mut s = self.lock();
        if let Some(sets) = s.pools.remove(pool) {
            for set in sets {
                s.sets.remove(set);
            }
        }
    }

    /// `vkResetDescriptorPool`: frees every set of the pool.
    pub fn reset_descriptor_pool(&self, pool: u64) -> VkResult<()> {
        let mut s = self.lock();
        let sets = std::mem::take(
            s.pools
                .get_mut(pool)
                .ok_or_else(|| self.bad("descriptor pool", pool))?,
        );
        for set in sets {
            s.sets.remove(set);
        }
        Ok(())
    }

    /// `vkAllocateDescriptorSets`. A binding with immutable samplers starts with them.
    pub fn allocate_sets(&self, pool: u64, layouts: &[u64]) -> VkResult<Vec<u64>> {
        let mut s = self.lock();
        if s.pools.get(pool).is_none() {
            return Err(self.bad("descriptor pool", pool));
        }
        let mut out = Vec::with_capacity(layouts.len());
        for &l in layouts {
            let Some(layout) = s.set_layouts.get(l).cloned() else {
                for h in out {
                    s.sets.remove(h);
                }
                return Err(self.bad("set layout", l));
            };
            let mut descriptors = vec![Descriptor::Empty; layout.total as usize];
            for b in &layout.bindings {
                for (i, &smp) in b.immutable.iter().enumerate() {
                    let at = (b.base as usize) + i;
                    if let Some(d) = descriptors.get_mut(at) {
                        *d = if b.ty == dt::SAMPLER {
                            Descriptor::Sampler(smp)
                        } else {
                            Descriptor::Image {
                                view: TextureId(0),
                                sampler: Some(smp),
                            }
                        };
                    }
                }
            }
            out.push(s.sets.insert(DescriptorSet {
                pool,
                layout,
                descriptors: Arc::new(descriptors),
            }));
        }
        if let Some(p) = s.pools.get_mut(pool) {
            p.extend(out.iter().copied());
        }
        Ok(out)
    }

    /// `vkFreeDescriptorSets`. A set that is gone (freed with its pool) is ignored, as the console's driver allows.
    pub fn free_sets(&self, sets: &[u64]) {
        let mut s = self.lock();
        for &h in sets {
            if let Some(set) = s.sets.remove(h)
                && let Some(p) = s.pools.get_mut(set.pool)
            {
                p.remove(&h);
            }
        }
    }

    /// `vkUpdateDescriptorSets`: the writes, then the copies. An element past a binding's count goes on into the next
    /// binding. A copy takes the source descriptor whole, but a destination with immutable samplers keeps its own.
    pub fn update_sets(&self, writes: &[WriteInfo], copies: &[CopyInfo]) {
        let mut guard = self.lock();
        let s = &mut *guard;
        for w in writes {
            let Some(set) = s.sets.get(w.set) else {
                self.log
                    .once("write set", "[RED] a descriptor write names an unknown set");
                continue;
            };
            let layout = set.layout.clone();
            let mut changes = Vec::with_capacity(w.items.len());
            for (j, item) in w.items.iter().enumerate() {
                let Some((at, b)) = layout.element(w.binding, w.element + j as u32) else {
                    self.log.once(
                        "write past end",
                        format_args!(
                            "[RED] write descriptor past the end of the layout (binding {})",
                            w.binding
                        ),
                    );
                    break;
                };
                let old = set
                    .descriptors
                    .get(at as usize)
                    .copied()
                    .unwrap_or_default();
                changes.push((
                    at as usize,
                    descriptor(&self.log, s, b, at - b.base, old, *item),
                ));
            }
            // Copy on write: a submit that holds the old contents keeps them.
            if let Some(set) = s.sets.get_mut(w.set) {
                let d = Arc::make_mut(&mut set.descriptors);
                for (at, v) in changes {
                    if let Some(slot) = d.get_mut(at) {
                        *slot = v;
                    }
                }
            }
        }
        for c in copies {
            let (Some(src), Some(dst)) = (s.sets.get(c.src.0), s.sets.get(c.dst.0)) else {
                continue;
            };
            let (sl, dl) = (src.layout.clone(), dst.layout.clone());
            let from = src.descriptors.clone();
            let mut changes = Vec::with_capacity(c.count as usize);
            for j in 0..c.count {
                let (Some((sa, _)), Some((da, db))) = (
                    sl.element(c.src.1, c.src.2 + j),
                    dl.element(c.dst.1, c.dst.2 + j),
                ) else {
                    break;
                };
                let Some(v) = from.get(sa as usize) else {
                    break;
                };
                let keep = db.immutable.get((da - db.base) as usize).copied();
                let v = match (*v, keep) {
                    (Descriptor::Image { view, .. }, Some(smp)) => Descriptor::Image {
                        view,
                        sampler: Some(smp),
                    },
                    (_, Some(smp)) if db.ty == dt::SAMPLER => Descriptor::Sampler(smp),
                    (v, _) => v,
                };
                changes.push((da as usize, v));
            }
            if let Some(set) = s.sets.get_mut(c.dst.0) {
                let d = Arc::make_mut(&mut set.descriptors);
                for (at, v) in changes {
                    if let Some(slot) = d.get_mut(at) {
                        *slot = v;
                    }
                }
            }
        }
    }

    /// `vkCreateDescriptorUpdateTemplate` (and `KHR`). Only the descriptor set type is implemented.
    pub fn create_update_template(&self, ty: u32, entries: Vec<TemplateEntry>) -> u64 {
        if ty != special::TEMPLATE_DESCRIPTOR_SET {
            self.log.once(
                "template type",
                format_args!("[RED] descriptor update template type {ty} not implemented"),
            );
        }
        self.lock().templates.insert(entries)
    }

    /// `vkDestroyDescriptorUpdateTemplate`.
    pub fn destroy_update_template(&self, template: u64) {
        self.lock().templates.remove(template);
    }

    /// `vkUpdateDescriptorSetWithTemplate` (and `KHR`): reads the game's `data` by the template's entries and writes
    /// the set.
    pub fn update_set_with_template(
        &self,
        m: &impl Mem,
        set: u64,
        template: u64,
        data: u64,
    ) -> VkResult<()> {
        let entries = self
            .lock()
            .templates
            .get(template)
            .cloned()
            .ok_or_else(|| self.bad("update template", template))?;
        let writes = template_writes(m, set, &entries, data)?;
        self.update_sets(&writes, &[]);
        Ok(())
    }

    /// `vkCreatePipelineLayout`: at most 8 sets and 256 bytes of push constants.
    pub fn create_pipeline_layout(&self, sets: &[u64], push_ranges: &[PushRange]) -> VkResult<u64> {
        if sets.len() > 8 {
            self.log.once(
                "layout sets",
                format_args!("[RED] pipeline layout with {} sets, over 8", sets.len()),
            );
        }
        let mut push_size = push_ranges
            .iter()
            .map(|r| r.offset.saturating_add(r.size))
            .max()
            .unwrap_or(0);
        if push_size > 256 {
            self.log.once(
                "push size",
                format_args!("[RED] push constant range of {push_size} bytes, over 256"),
            );
            push_size = 256;
        }
        let mut s = self.lock();
        let layouts = sets
            .iter()
            .take(8)
            .map(|&h| s.set_layouts.get(h).cloned().unwrap_or_default())
            .collect();
        Ok(s.pipeline_layouts.insert(Arc::new(PipelineLayoutDesc {
            sets: layouts,
            push_ranges: push_ranges.to_vec(),
            push_size,
        })))
    }

    /// `vkDestroyPipelineLayout`: pipelines keep what they use.
    pub fn destroy_pipeline_layout(&self, layout: u64) {
        self.lock().pipeline_layouts.remove(layout);
    }
}
