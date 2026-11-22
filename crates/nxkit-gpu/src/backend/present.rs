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
        let (texture, scaled) = self
            .lock_gpu()
            .texture_scaled(present.texture)
            .ok_or_else(|| BackendError("the presented texture has no Metal texture".into()))?;
        self.diag.ready(number);
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
            crop: if scaled {
                scale_crop(present.crop, self.scale)
            } else {
                present.crop
            },
            origin_lower_left,
        });
        if let Some(p) = self.pacer.get() {
            p.game_frame_ready(number);
        }
        Ok(())
    }
}

/// The crop of a window texture that renders at a scale. The game gives the crop in its own pixels
/// (`nvnWindowSetCrop`), and the scaled texture holds the same picture `scale` times larger. Pokopia crops 1920x1080 of
/// its window, and at scale 2 the blit showed the top left quarter of the picture.
fn scale_crop(crop: nxkit_nvn::backend::Rect, scale: f32) -> nxkit_nvn::backend::Rect {
    let up = |v: i32| (v as f32 * scale).round() as i32;
    nxkit_nvn::backend::Rect {
        x: up(crop.x),
        y: up(crop.y),
        width: up(crop.width),
        height: up(crop.height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_nvn::backend::Rect;

    #[test]
    fn the_crop_of_a_scaled_window_texture_scales_with_it() {
        let crop = Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let at2 = Rect {
            x: 0,
            y: 0,
            width: 3840,
            height: 2160,
        };
        assert_eq!(scale_crop(crop, 2.0), at2);
        // No crop stays no crop.
        assert_eq!(scale_crop(Rect::default(), 2.0), Rect::default());
    }
}
