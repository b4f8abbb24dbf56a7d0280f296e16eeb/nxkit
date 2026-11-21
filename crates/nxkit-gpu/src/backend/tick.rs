//! The display tick of [`GpuBackend`]: the `present` counters and the `TickSink` that draws the shown texture.

use super::*;

impl TickSink for GpuBackend {
    fn tick(&self, action: TickAction, tick: DisplayTick) {
        self.diag.count(0);
        let TickAction::Present(shown) = action else {
            return;
        };
        self.diag.count(1);
        let number = match shown {
            Shown::Game { frame } => frame,
            Shown::Interpolated { to, .. } => to,
        };
        self.diag.shown(number, locked(&self.pending).len());
        let texture = {
            let mut pending = locked(&self.pending);
            let found = pending.iter().position(|p| p.number == number).map(|i| {
                let p = pending.remove(i);
                (p.texture, p.crop, p.origin_lower_left)
            });
            for p in pending.iter().filter(|p| p.number < number) {
                self.screen.shown(p.number, -1.0);
            }
            pending.retain(|p| p.number > number);
            self.shown_upto.fetch_max(number, Ordering::AcqRel);
            self.freed.notify_all();
            found
        };
        let Some((texture, crop, origin_lower_left)) = texture else {
            return;
        };
        self.diag.count(2);
        if self.diag.found() % 300 == 1 {
            eprintln!("DIAG present texture {:?}", texture.desc());
        }
        if crate::diag("pixelall") && self.diag.found().is_multiple_of(10) {
            let d = texture.desc();
            let pts: Vec<[u32; 2]> = (1..4)
                .flat_map(|j| (1..4).map(move |i| [d.width * i / 4, d.height * j / 4]))
                .collect();
            let px = texture.debug_read(&self.ctx, &pts).unwrap_or_default();
            let lit = px.iter().filter(|p| p[0] | p[1] | p[2] > 8).count();
            eprintln!("DIAG pixelall present {} lit {lit}/9", self.diag.found());
        }
        if crate::diag("pixel") && self.diag.found() % 60 == 1 {
            let d = texture.desc();
            let pts: Vec<[u32; 2]> = (1..4)
                .flat_map(|j| (1..4).map(move |i| [d.width * i / 4, d.height * j / 4]))
                .collect();
            let px = texture.debug_read(&self.ctx, &pts).unwrap_or_default();
            let lit = px.iter().filter(|p| p[0] | p[1] | p[2] != 0).count();
            eprintln!(
                "DIAG pixel {}x{} {:?} lit {lit}/9 {px:?}",
                d.width, d.height, d.format
            );
        }
        let mut blit = locked(&self.blit);
        if blit.is_none() {
            *blit = Blit::new(&self.ctx, &self.timed)
                .inspect_err(|e| {
                    if !self.blit_failed.swap(true, Ordering::Relaxed) {
                        eprintln!("nxkit-gpu: the present blit cannot be made: {e}");
                    }
                })
                .ok();
        }
        let Some(b) = blit.as_mut() else {
            return;
        };
        if matches!(shown, Shown::Game { .. }) {
            let screen = self.screen.clone();
            tick.drawable
                .on_presented(move |secs| screen.shown(number, secs));
        }
        let r = b.present(
            &texture,
            crop,
            origin_lower_left,
            tick.drawable,
            // A tick of the immediate mode has no target: the frame shows as soon as it is drawn.
            (!tick.immediate).then_some(tick.target_presentation_timestamp),
        );
        if r.is_ok() {
            self.diag.count(3);
        }
        if let Err(e) = r
            && !self.blit_failed.swap(true, Ordering::Relaxed)
        {
            eprintln!("nxkit-gpu: the present failed: {e}");
        }
    }
}
