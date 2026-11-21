//! A submit: the replay of its command buffers into one Metal 4 command buffer, and its completion (the reference
//! layer's `vkQueueSubmit`, `submit_segment` and `conv_sync`).

use std::sync::Arc;

use nxkit_metal::{CommandBuffer, GuestMemory, Scratch, TextureRegion};
use nxkit_vk::backend::{BackendError, BackendResult, Submission};
use nxkit_vk::cmd::Cmd;
use nxkit_vk::desc::MemoryId;

use super::{VkGpu, locked};
use crate::objects::Objects;
use crate::replay::{self, Replay, Stats, Tables};
use crate::{convert, passplan, queries};

impl VkGpu {
    /// Replays a submit and commits it. The serial completes when the GPU is done, or at once (after the earlier ones)
    /// when the submit cannot be encoded.
    pub(super) fn submit_now(&self, sub: &Submission) -> BackendResult<()> {
        self.drop_retired();
        let mut q = locked(&self.queue);
        let mut frame = match q.ring.begin() {
            Ok(f) => f,
            Err(e) => {
                self.done.finish(sub.serial);
                return Err(BackendError(format!("submit {}: {e}", sub.serial)));
            }
        };
        let scratch = frame.scratch();
        let mut flat: Vec<Vec<&Cmd>> = Vec::with_capacity(sub.command_buffers.len());
        for c in &sub.command_buffers {
            let mut out = Vec::new();
            passplan::flatten(&c.cmds, &sub.secondaries, &mut out, 0);
            flat.push(out);
        }
        let ops = queries::ops(flat.iter().flatten().copied());
        let (stats, writes) = {
            let objects = locked(&self.objects);
            self.sync_converted(&objects, frame.commands(), &scratch, &q.tables);
            let mut r = Replay {
                ctx: &self.ctx,
                objects: &objects,
                sets: &sub.sets,
                tables: &q.tables,
                scratch: scratch.clone(),
                log: &self.log,
                stats: Stats::default(),
            };
            for cmds in &flat {
                replay::primary(&mut r, frame.commands(), cmds);
            }
            // the memories that the query copies write, resolved now: the completion has no access to the objects
            let writes: Vec<(MemoryId, Option<Arc<GuestMemory>>)> = ops
                .iter()
                .filter_map(|o| match o {
                    queries::QueryOp::Copy { dst, .. } => {
                        Some((dst.memory, objects.memories.get(&dst.memory).cloned()))
                    }
                    _ => None,
                })
                .collect();
            (r.stats, writes)
        };
        self.count(stats);
        let serial = sub.serial;
        let done = self.done.clone();
        let pools = self.queries.clone();
        let epoch = self.epoch;
        frame.commands().after_gpu(move |ok| {
            if !ops.is_empty() {
                let now = u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
                let bytes = queries::apply(&mut super::locked(&pools), &ops, now);
                for (at, data) in bytes {
                    let mem = writes
                        .iter()
                        .find(|(id, _)| *id == at.memory)
                        .and_then(|(_, m)| m.as_ref());
                    if let Some(m) = mem {
                        let _ = m.write(at.offset, &data);
                    }
                }
            }
            if !ok {
                eprintln!("[vk] [RED] the command buffer of submit {serial} failed on the GPU");
            }
            done.finish(serial);
        });
        // a texture made by another thread after the ring committed the residency set may be used here
        self.ctx.commit_residency();
        match frame.submit() {
            Ok(_) => {
                q.committed = serial;
                Ok(())
            }
            Err(e) => {
                // the dropped command buffer runs its completion with `false`, which finishes the serial
                Err(BackendError(format!("submit {serial}: {e}")))
            }
        }
    }

    /// Unpacks the converted linear images whose memory changed since they were last unpacked, in a compute pass
    /// before the commands of the submit.
    fn sync_converted(
        &self,
        objects: &Objects,
        cb: &mut CommandBuffer,
        scratch: &Arc<Scratch>,
        tables: &Tables,
    ) {
        let mut uploads = Vec::new();
        for id in &objects.converted {
            let Some(tex) = objects.textures.get(id) else {
                continue;
            };
            let Some(conv) = &tex.converted else {
                continue;
            };
            let mut c = locked(conv);
            let mut now = vec![0u8; c.shadow.len()];
            if c.memory.read(c.offset, &mut now).is_err() || now == c.shadow {
                continue;
            }
            let d = tex.texture.desc();
            let (w, h) = (d.width as usize, d.height as usize);
            let texels = convert::unpack(c.conversion, &now, c.pitch as usize, w, h);
            c.shadow = now;
            let bpr = (w * convert::texel_bytes(c.conversion)) as u32;
            uploads.push((tex.texture.clone(), texels, bpr));
        }
        if uploads.is_empty() {
            return;
        }
        let Ok(mut pass) = cb.begin_compute_pass(&tables.compute) else {
            return;
        };
        let _ = pass.encode(&nxkit_metal::ComputeCommand::WaitForEarlierPasses);
        for (texture, texels, bpr) in uploads {
            let d = texture.desc();
            let region = TextureRegion::whole(0, 0, [d.width, d.height, 1]);
            let ok = scratch.push(&texels).and_then(|addr| {
                pass.copy_scratch_to_texture(scratch, addr, bpr, &texture, &region)
            });
            if let Err(e) = ok {
                self.log.once(
                    "convert",
                    format_args!("[RED] a converted image cannot be filled: {e}"),
                );
            }
        }
        pass.end();
    }

    /// Adds the counters of a submit and prints them now and then.
    fn count(&self, s: Stats) {
        let mut t = locked(&self.totals);
        t.0.draws += s.draws;
        t.0.skipped += s.skipped;
        t.0.passes += s.passes;
        t.0.copies += s.copies;
        t.0.dispatches += s.dispatches;
        t.1 += 1;
    }

    /// Prints the counters at present `n`: the first three presents and every 300th.
    pub(super) fn report(&self, n: u64) {
        if n > 3 && !n.is_multiple_of(300) {
            return;
        }
        let (s, submits) = *locked(&self.totals);
        eprintln!(
            "[vk] present {n}: draws {} (skipped {}), dispatches {}, passes {}, copies {}, submits {submits} so far",
            s.draws, s.skipped, s.dispatches, s.passes, s.copies
        );
    }
}
