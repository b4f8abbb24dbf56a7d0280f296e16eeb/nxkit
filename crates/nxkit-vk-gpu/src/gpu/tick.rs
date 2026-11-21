//! The display tick: the `TickSink` that blits the shown swapchain image into the drawable.

use std::sync::atomic::Ordering;

use nxkit_metal::{DisplayTick, Shown, TickAction, TickSink};

use super::{VkGpu, locked};
use crate::blit::Blit;

impl TickSink for VkGpu {
    fn tick(&self, action: TickAction, tick: DisplayTick) {
        let TickAction::Present(shown) = action else {
            return;
        };
        let number = match shown {
            Shown::Game { frame } => frame,
            Shown::Interpolated { to, .. } => to,
        };
        let texture = {
            let mut p = locked(&self.presents);
            let found = p
                .pending
                .iter()
                .position(|x| x.number == number)
                .map(|i| p.pending.remove(i).texture);
            p.pending.retain(|x| x.number > number);
            p.shown_upto = p.shown_upto.max(number);
            found
        };
        self.freed.notify_all();
        let Some(texture) = texture else {
            return;
        };
        let mut blit = locked(&self.blit);
        if blit.is_none() {
            *blit = Blit::new(&self.ctx, self.settings.timed_present.as_ref())
                .inspect_err(|e| {
                    if !self.blit_failed.swap(true, Ordering::Relaxed) {
                        eprintln!("[vk] [RED] the present blit cannot be made: {e}");
                    }
                })
                .ok();
        }
        let Some(b) = blit.as_mut() else {
            return;
        };
        if let (Some(hook), Shown::Game { .. }) = (&self.settings.on_shown, shown) {
            let hook = hook.clone();
            tick.drawable.on_presented(move |secs| hook(number, secs));
        }
        // A tick of the immediate mode has no target: the frame shows as soon as it is drawn.
        let at = (!tick.immediate).then_some(tick.target_presentation_timestamp);
        if let Err(e) = b.present(&texture, tick.drawable, at)
            && !self.blit_failed.swap(true, Ordering::Relaxed)
        {
            eprintln!("[vk] [RED] the present failed: {e}");
        }
    }
}
