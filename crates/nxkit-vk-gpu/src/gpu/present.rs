//! Acquire and present (the reference layer's `vkAcquireNextImageKHR` and `vkQueuePresentKHR`, paced like the NVN
//! backend's `acquire` and `present_now`).
//!
//! A present queues the swapchain image and tells the pacer. The image was drawn by submits that are already
//! committed to the queue, and the blit of the tick is committed after them to the same queue and waits for the earlier
//! work, so the picture it shows is the one of the serial it follows.

use std::time::{Duration, Instant};

use nxkit_vk::backend::{BackendError, BackendResult, Present};
use nxkit_vk::desc::TextureId;

use super::{ACQUIRE_WAIT, MAX_PENDING, Pending, VkGpu, locked};

impl VkGpu {
    /// Paces an acquire: waits while [`MAX_PENDING`] presents wait for a tick, and until the previous present of
    /// `image` has been shown. A wait of 100 ms without a tick drops the oldest present, so a window that shows nothing
    /// does not stop the game. Gives false when `timeout_ns` passed first.
    pub(super) fn acquire_now(&self, image: TextureId, timeout_ns: u64) -> bool {
        let deadline = Instant::now().checked_add(Duration::from_nanos(timeout_ns));
        let mut p = locked(&self.presents);
        loop {
            let image_busy = p.last.get(&image).is_some_and(|&n| n > p.shown_upto);
            if p.pending.len() < MAX_PENDING && !image_busy {
                return true;
            }
            let slice = match deadline {
                Some(d) => match d.checked_duration_since(Instant::now()) {
                    Some(left) if !left.is_zero() => left.min(ACQUIRE_WAIT),
                    _ => return false,
                },
                None => ACQUIRE_WAIT,
            };
            let (guard, t) = self
                .freed
                .wait_timeout(p, slice)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            p = guard;
            if t.timed_out() && slice == ACQUIRE_WAIT && !p.pending.is_empty() {
                let old = p.pending.remove(0);
                p.shown_upto = p.shown_upto.max(old.number);
            }
        }
    }

    /// Queues a present for the pacer.
    pub(super) fn present_now(&self, present: &Present) -> BackendResult<()> {
        let texture = locked(&self.objects)
            .texture(present.image)
            .cloned()
            .ok_or_else(|| BackendError("the presented image has no texture".into()))?;
        if present.after > locked(&self.queue).committed {
            self.log.once(
                "present ahead",
                "[RED] a present follows a submit that was not committed",
            );
        }
        let pacer = self.pacer.get().cloned();
        let number = {
            let mut p = locked(&self.presents);
            p.presented += 1;
            let n = p.presented;
            p.last.insert(present.image, n);
            match &pacer {
                Some(_) => p.pending.push(Pending { number: n, texture }),
                // no display: the present counts as shown
                None => p.shown_upto = n,
            }
            n
        };
        self.freed.notify_all();
        self.report(number);
        if let Some(pacer) = pacer {
            pacer.game_frame_ready(number);
        }
        Ok(())
    }
}
