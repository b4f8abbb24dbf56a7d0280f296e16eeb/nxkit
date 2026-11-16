//! The display tick of [`GpuBackend`]: the `present` counters and the `TickSink` that draws the shown texture.

use super::*;

/// Counters of the present path for the `present` diagnostics: ticks, presents asked, textures found, blits done.
static PRESENT_DIAG: [std::sync::atomic::AtomicU64; 4] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 4];

fn present_diag(i: usize) {
    if !crate::diag("present") {
        return;
    }
    let n = PRESENT_DIAG[i].fetch_add(1, Ordering::Relaxed) + 1;
    if i == 0 && n.is_multiple_of(120) {
        let v: Vec<u64> = PRESENT_DIAG
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .collect();
        eprintln!(
            "DIAG present ticks {} asked {} found {} blitted {}",
            v[0], v[1], v[2], v[3]
        );
    }
}

impl TickSink for GpuBackend {
    fn tick(&self, action: TickAction, tick: DisplayTick) {
        present_diag(0);
        let TickAction::Present(shown) = action else {
            return;
        };
        present_diag(1);
        let number = match shown {
            Shown::Game { frame } => frame,
            Shown::Interpolated { to, .. } => to,
        };
        encpace::shown(number, locked(&self.pending).len());
        let texture = {
            let mut pending = locked(&self.pending);
            let found = pending.iter().position(|p| p.number == number).map(|i| {
                let p = pending.remove(i);
                (p.texture, p.crop, p.origin_lower_left)
            });
            for p in pending.iter().filter(|p| p.number < number) {
                nxkit_nvn::screen::shown(p.number, -1.0);
            }
            pending.retain(|p| p.number > number);
            self.shown_upto.fetch_max(number, Ordering::AcqRel);
            self.freed.notify_all();
            found
        };
        let Some((texture, crop, origin_lower_left)) = texture else {
            return;
        };
        present_diag(2);
        if crate::diag("present") && PRESENT_DIAG[2].load(Ordering::Relaxed) % 300 == 1 {
            eprintln!("DIAG present texture {:?}", texture.desc());
        }
        if crate::diag("pixelall") && PRESENT_DIAG[2].load(Ordering::Relaxed).is_multiple_of(10) {
            let d = texture.desc();
            let pts: Vec<[u32; 2]> = (1..4)
                .flat_map(|j| (1..4).map(move |i| [d.width * i / 4, d.height * j / 4]))
                .collect();
            let px = texture.debug_read(&self.ctx, &pts).unwrap_or_default();
            let lit = px.iter().filter(|p| p[0] | p[1] | p[2] > 8).count();
            eprintln!(
                "DIAG pixelall present {} lit {lit}/9",
                PRESENT_DIAG[2].load(Ordering::Relaxed)
            );
        }
        if crate::diag("pixel") && PRESENT_DIAG[2].load(Ordering::Relaxed) % 60 == 1 {
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
            *blit = Blit::new(&self.ctx)
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
            tick.drawable
                .on_presented(move |secs| nxkit_nvn::screen::shown(number, secs));
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
            present_diag(3);
        }
        if let Err(e) = r
            && !self.blit_failed.swap(true, Ordering::Relaxed)
        {
            eprintln!("nxkit-gpu: the present failed: {e}");
        }
    }
}
