//! The recording of copies, blits, resolves, fills, clears, secondary command buffers, queries, events and barriers.

use crate::cmd::{
    BufferCopy, BufferImageCopy, ClearAttachment, ClearRect, ClearValue, Cmd, ImageBlit, ImageCopy,
};
use crate::consts::{buffer_usage, image_usage, query_result, special::WHOLE_SIZE};
use crate::desc::{BufferRef, QueryPoolId};
use crate::info::SubresourceRange;

use super::Vk;
use super::state::{Image, State};

/// `GENERAL`, `TRANSFER_SRC_OPTIMAL` or `SHARED_PRESENT_KHR` for a source; `TRANSFER_DST_OPTIMAL` for a destination.
fn transfer_layout(layout: u32, dst: bool) -> bool {
    let own = if dst { 7 } else { 6 };
    layout == 1 || layout == own || layout == 1_000_111_000
}

/// The checks of the reference layer for a blit or a resolve: transfer usage and layouts.
fn transfer_images(
    s: &State,
    src: u64,
    src_layout: u32,
    dst: u64,
    dst_layout: u32,
) -> Option<(Image, Image)> {
    let (a, b) = (s.images.get(src)?, s.images.get(dst)?);
    let ok = a.desc.usage & image_usage::TRANSFER_SRC != 0
        && b.desc.usage & image_usage::TRANSFER_DST != 0
        && transfer_layout(src_layout, false)
        && transfer_layout(dst_layout, true);
    ok.then(|| (a.clone(), b.clone()))
}

impl Vk {
    /// `vkCmdCopyBuffer`.
    pub fn cmd_copy_buffer(&self, cb: u64, src: u64, dst: u64, regions: Vec<BufferCopy>) {
        self.record(cb, |s| {
            Some(Cmd::CopyBuffer {
                src: s.buffer_ref(&self.log, src, 0),
                dst: s.buffer_ref(&self.log, dst, 0),
                regions,
            })
        });
    }

    /// `vkCmdCopyBufferToImage`.
    pub fn cmd_copy_buffer_to_image(
        &self,
        cb: u64,
        src: u64,
        dst: u64,
        regions: Vec<BufferImageCopy>,
    ) {
        self.record(cb, |s| {
            Some(Cmd::CopyBufferToImage {
                src: s.buffer_ref(&self.log, src, 0),
                dst: s.image_ref(&self.log, dst),
                regions,
            })
        });
    }

    /// `vkCmdCopyImageToBuffer`.
    pub fn cmd_copy_image_to_buffer(
        &self,
        cb: u64,
        src: u64,
        dst: u64,
        regions: Vec<BufferImageCopy>,
    ) {
        self.record(cb, |s| {
            Some(Cmd::CopyImageToBuffer {
                src: s.image_ref(&self.log, src),
                dst: s.buffer_ref(&self.log, dst, 0),
                regions,
            })
        });
    }

    /// `vkCmdCopyImage`.
    pub fn cmd_copy_image(&self, cb: u64, src: u64, dst: u64, regions: Vec<ImageCopy>) {
        self.record(cb, |s| {
            Some(Cmd::CopyImage {
                src: s.image_ref(&self.log, src),
                dst: s.image_ref(&self.log, dst),
                regions,
            })
        });
    }

    /// `vkCmdBlitImage`: single-sample images with transfer usage and layouts, nearest or linear filter.
    #[allow(clippy::too_many_arguments)]
    pub fn cmd_blit_image(
        &self,
        cb: u64,
        src: u64,
        src_layout: u32,
        dst: u64,
        dst_layout: u32,
        regions: Vec<ImageBlit>,
        filter: u32,
    ) {
        self.record(cb, |s| {
            let pair = transfer_images(s, src, src_layout, dst, dst_layout)
                .filter(|(a, b)| a.desc.samples == 1 && b.desc.samples == 1 && filter <= 1);
            let Some((a, b)) = pair else {
                self.log.once(
                    "blit",
                    "[RED] vkCmdBlitImage usage, layout, sample count or filter",
                );
                return None;
            };
            Some(Cmd::BlitImage {
                src: a.as_ref(src),
                dst: b.as_ref(dst),
                regions,
                filter,
            })
        });
    }

    /// `vkCmdResolveImage`: a multisample source and a single-sample destination of the same format.
    pub fn cmd_resolve_image(
        &self,
        cb: u64,
        src: u64,
        src_layout: u32,
        dst: u64,
        dst_layout: u32,
        regions: Vec<ImageCopy>,
    ) {
        self.record(cb, |s| {
            let pair = transfer_images(s, src, src_layout, dst, dst_layout).filter(|(a, b)| {
                a.desc.format == b.desc.format && a.desc.samples > 1 && b.desc.samples == 1
            });
            let Some((a, b)) = pair else {
                self.log.once(
                    "resolve",
                    "[RED] vkCmdResolveImage usage, layout, format or sample count",
                );
                return None;
            };
            Some(Cmd::ResolveImage {
                src: a.as_ref(src),
                dst: b.as_ref(dst),
                regions,
            })
        });
    }

    /// `vkCmdFillBuffer`: `VK_WHOLE_SIZE` fills to the end, in whole words.
    pub fn cmd_fill_buffer(&self, cb: u64, dst: u64, offset: u64, size: u64, data: u32) {
        self.record(cb, |s| {
            let r = s.buffer_ref(&self.log, dst, offset);
            let size = if size == WHOLE_SIZE {
                r.size & !3
            } else {
                size
            };
            Some(Cmd::FillBuffer {
                dst: BufferRef { size, ..r },
                data,
            })
        });
    }

    /// `vkCmdUpdateBuffer`: at most 65536 bytes, in whole words, inside a buffer with transfer destination usage.
    pub fn cmd_update_buffer(&self, cb: u64, dst: u64, offset: u64, data: Vec<u8>) {
        self.record(cb, |s| {
            let size = data.len() as u64;
            let ok = s.buffers.get(dst).is_some_and(|b| {
                b.info.usage & buffer_usage::TRANSFER_DST != 0
                    && size > 0
                    && size <= 65536
                    && (offset | size) & 3 == 0
                    && offset <= b.info.size
                    && size <= b.info.size - offset
            });
            if !ok {
                self.log.once(
                    "update buffer",
                    "[RED] vkCmdUpdateBuffer destination usage, alignment or range",
                );
                return None;
            }
            let r = s.buffer_ref(&self.log, dst, offset);
            Some(Cmd::UpdateBuffer {
                dst: BufferRef { size, ..r },
                data,
            })
        });
    }

    fn ranges_of(s: &State, image: u64, ranges: Vec<SubresourceRange>) -> Vec<SubresourceRange> {
        match s.images.get(image) {
            Some(i) => ranges
                .into_iter()
                .map(|r| r.resolve(i.desc.levels, i.desc.layers))
                .collect(),
            None => ranges,
        }
    }

    /// `vkCmdClearColorImage`.
    pub fn cmd_clear_color_image(
        &self,
        cb: u64,
        image: u64,
        color: ClearValue,
        ranges: Vec<SubresourceRange>,
    ) {
        self.record(cb, |s| {
            Some(Cmd::ClearColorImage {
                image: s.image_ref(&self.log, image),
                color,
                ranges: Self::ranges_of(s, image, ranges),
            })
        });
    }

    /// `vkCmdClearDepthStencilImage`.
    pub fn cmd_clear_depth_stencil_image(
        &self,
        cb: u64,
        image: u64,
        depth: f32,
        stencil: u32,
        ranges: Vec<SubresourceRange>,
    ) {
        self.record(cb, |s| {
            Some(Cmd::ClearDepthStencilImage {
                image: s.image_ref(&self.log, image),
                depth,
                stencil,
                ranges: Self::ranges_of(s, image, ranges),
            })
        });
    }

    /// `vkCmdClearAttachments`.
    pub fn cmd_clear_attachments(
        &self,
        cb: u64,
        attachments: Vec<ClearAttachment>,
        rects: Vec<ClearRect>,
    ) {
        self.record(cb, |_| Some(Cmd::ClearAttachments { attachments, rects }));
    }

    /// `vkCmdExecuteCommands`: the secondary command buffers by handle; their commands are read at submit.
    pub fn cmd_execute_commands(&self, cb: u64, secondaries: &[u64]) {
        self.record(cb, |_| Some(Cmd::ExecuteCommands(secondaries.to_vec())));
    }

    /// `vkCmdResetQueryPool`.
    pub fn cmd_reset_query_pool(&self, cb: u64, pool: u64, first: u32, count: u32) {
        self.record(cb, |_| {
            Some(Cmd::ResetQueryPool {
                pool: QueryPoolId(pool),
                first,
                count,
            })
        });
    }

    /// `vkCmdBeginQuery`.
    pub fn cmd_begin_query(&self, cb: u64, pool: u64, query: u32, flags: u32) {
        self.record(cb, |_| {
            Some(Cmd::BeginQuery {
                pool: QueryPoolId(pool),
                query,
                flags,
            })
        });
    }

    /// `vkCmdEndQuery`.
    pub fn cmd_end_query(&self, cb: u64, pool: u64, query: u32) {
        self.record(cb, |_| {
            Some(Cmd::EndQuery {
                pool: QueryPoolId(pool),
                query,
            })
        });
    }

    /// `vkCmdWriteTimestamp`.
    pub fn cmd_write_timestamp(&self, cb: u64, pool: u64, query: u32) {
        self.record(cb, |_| {
            Some(Cmd::WriteTimestamp {
                pool: QueryPoolId(pool),
                query,
            })
        });
    }

    /// `vkCmdCopyQueryPoolResults`: the range, the destination usage and the alignment are checked as Vulkan says.
    #[allow(clippy::too_many_arguments)]
    pub fn cmd_copy_query_pool_results(
        &self,
        cb: u64,
        pool: u64,
        first: u32,
        count: u32,
        dst: u64,
        offset: u64,
        stride: u64,
        flags: u32,
    ) {
        if count == 0 {
            return;
        }
        self.record(cb, |s| {
            let word = if flags & query_result::R64 != 0 { 8 } else { 4 };
            let row = word
                * if flags & query_result::WITH_AVAILABILITY != 0 {
                    2
                } else {
                    1
                };
            let n = s
                .query_pools
                .get(pool)
                .map_or(0, |q| q.tickets.len() as u32);
            let ok = s.buffers.get(dst).is_some_and(|b| {
                let size = b.info.size;
                first <= n
                    && count <= n - first
                    && b.info.usage & buffer_usage::TRANSFER_DST != 0
                    && offset.is_multiple_of(word)
                    && offset <= size
                    && row <= size - offset
                    && (count == 1
                        || (stride >= row
                            && stride.is_multiple_of(word)
                            && stride <= (size - offset - row) / u64::from(count - 1)))
            });
            if !ok {
                self.log.once(
                    "query copy",
                    "[RED] query copy range, destination usage or alignment",
                );
                return None;
            }
            Some(Cmd::CopyQueryPoolResults {
                pool: QueryPoolId(pool),
                first,
                count,
                dst: s.buffer_ref(&self.log, dst, offset),
                stride,
                flags,
            })
        });
    }

    /// `vkCmdSetEvent`.
    pub fn cmd_set_event(&self, cb: u64, event: u64) {
        self.record(cb, |_| Some(Cmd::SetEvent(event)));
    }

    /// `vkCmdResetEvent`.
    pub fn cmd_reset_event(&self, cb: u64, event: u64) {
        self.record(cb, |_| Some(Cmd::ResetEvent(event)));
    }

    /// `vkCmdWaitEvents`: one serial queue runs every command in order, so a wait for an event set earlier is met.
    /// Nothing is recorded.
    pub fn cmd_wait_events(&self, _cb: u64) {}

    /// `vkCmdPipelineBarrier`: recorded as a cut point when it has a barrier, nothing otherwise.
    pub fn cmd_pipeline_barrier(&self, cb: u64, barriers: u32) {
        if barriers > 0 {
            self.record(cb, |_| Some(Cmd::PipelineBarrier));
        }
    }

    /// The debug label and marker commands (`vkCmdBeginDebugUtilsLabelEXT`, `vkCmdDebugMarkerBeginEXT`, ...): nothing.
    pub fn cmd_debug_label(&self, _cb: u64) {}
}
