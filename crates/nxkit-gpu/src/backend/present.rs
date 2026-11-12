//! The image and the origin captured for one queued present.

use super::*;

impl GpuBackend {
    /// Keeps the Metal texture of the presented frame and tells the pacer. Runs after the encode of that frame.
    pub(super) fn present_now(
        &self,
        present: Present,
        origin_lower_left: bool,
    ) -> BackendResult<()> {
        // The number goes first, so that a present without a texture still counts as one of the game's presents.
        let number = self.presented.fetch_add(1, Ordering::AcqRel) + 1;
        let texture = self
            .lock_gpu()
            .texture(present.texture)
            .ok_or_else(|| BackendError("the presented texture has no Metal texture".into()))?;
        encpace::ready(number);
        if crate::diag("present") && number % 120 == 1 {
            eprintln!(
                "DIAG present id {:?} {}x{} {:?}",
                present.texture,
                texture.desc().width,
                texture.desc().height,
                texture.desc().format
            );
        }
        locked(&self.pending).push(Pending {
            number,
            texture,
            crop: present.crop,
            origin_lower_left,
        });
        if let Some(p) = self.pacer.get() {
            p.game_frame_ready(number);
        }
        Ok(())
    }
}
