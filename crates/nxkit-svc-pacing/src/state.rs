//! The frame step state page.
//!
//! A game whose frame step data is applied (Breath of the Wild) has one page in its module that the host and the game's
//! patched code share. The host writes the target frame rate, the render scale and the step of each frame. The game's
//! stubs read them. The rewrite data of the title names the place of the page in the module, and the host gives the
//! address to [`crate::Config::state_page`].
//!
//! The layout is the one of `dm/host/loader.c` and the patch tables of the engine
//! `0f2a772c13e2caed2959d1d1a93f8fe57f44c754`, read on 2026-11-07. It is listed in `THIRD_PARTY.md`. The offset `0x54`
//! belongs to the game's stubs, which latch the variable step there. The host never writes it.

use nxkit_abi::guest::{self, Fault};

/// The bytes of the page that the host reads and writes.
pub const SIZE: u64 = 0x60;

/// The offsets of the fields.
pub mod offset {
    /// `f32` target frame rate. 0 is the game's own step, 30 and 60 a step in whole vsyncs, 61 to 240 a variable step.
    pub const TARGET_FPS: u64 = 0x00;
    /// `u32` 1 while a prerendered movie plays.
    pub const MOVIE: u64 = 0x04;
    /// `f32` render scale of the 3D views.
    pub const RENDER_SCALE: u64 = 0x10;
    /// `u32` step in vsyncs, written by the host once a frame.
    pub const STEP_VSYNCS: u64 = 0x20;
    /// `u32` the game's own override of the step, which the host only reads.
    pub const OWN_STEP: u64 = 0x24;
    /// `u64` vsyncs the game has stepped.
    pub const STEPPED: u64 = 0x30;
    /// `u64` output size, height in the high half and width in the low half. 0 is the game's own size.
    pub const OUTPUT_SIZE: u64 = 0x40;
    /// `f32` output width. 0 is the game's own width.
    pub const OUTPUT_WIDTH: u64 = 0x48;
    /// `u32` factor that the render buffer pool grows by. 0 is the game's own size.
    pub const POOL_FACTOR: u64 = 0x4c;
    /// `f32` variable step in units of 1/30 s. 0 is the whole vsync step.
    pub const VARIABLE_STEP: u64 = 0x50;
    /// `u64` bytes that the game's heaps around the render buffer pool grow by.
    pub const EXTRA_MEMORY: u64 = 0x58;
}

/// The memory added around a grown render buffer pool, besides the growth of the pool itself.
const POOL_HEADROOM: u64 = 64 << 20;
/// The granularity of the added memory.
const EXTRA_ALIGN: u64 = 2 << 20;

/// The factor that the render buffer pool grows by for a render scale: the square of the scale rounded up.
pub fn pool_factor(scale: f32) -> u32 {
    if scale <= 1.0 {
        return 1;
    }
    (scale * scale).ceil() as u32
}

/// The bytes that the heaps around the pool grow by for a pool of `pool_bytes` that grows by `factor`.
pub fn extra_memory(pool_bytes: u64, factor: u32) -> u64 {
    let growth = u64::from(factor.saturating_sub(1)) * pool_bytes;
    (growth + POOL_HEADROOM + EXTRA_ALIGN - 1) & !(EXTRA_ALIGN - 1)
}

/// The output size that a render scale above the game's own gives: the views' base size times the scale, rounded up to
/// an even number. Returns `None` while the views fit in the game's output.
pub fn output_for(scale: f32, view: (u32, u32), game_output_width: u32) -> Option<(u32, u32)> {
    if view.0 == 0
        || game_output_width == 0
        || scale * view.0 as f32 <= game_output_width as f32 + 0.5
    {
        return None;
    }
    let even = |v: f32| (((v + 0.5) as u32) + 1) & !1;
    Some((even(scale * view.0 as f32), even(scale * view.1 as f32)))
}

/// The state page at an address in guest memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatePage {
    base: u64,
}

impl StatePage {
    /// The page at `base`. Fails when the page is not inside guest memory.
    pub fn new(base: u64) -> Result<Self, Fault> {
        guest::check(base, SIZE)?;
        Ok(Self { base })
    }

    /// The guest address of the page.
    pub fn base(&self) -> u64 {
        self.base
    }

    /// Sets the target frame rate.
    pub fn set_target_fps(&self, fps: f32) -> Result<(), Fault> {
        guest::write_f32(self.base + offset::TARGET_FPS, fps)
    }

    /// The target frame rate.
    pub fn target_fps(&self) -> Result<f32, Fault> {
        guest::read_f32(self.base + offset::TARGET_FPS)
    }

    /// Tells the game's stubs that a prerendered movie plays.
    pub fn set_movie_playing(&self, playing: bool) -> Result<(), Fault> {
        guest::write_u32(self.base + offset::MOVIE, u32::from(playing))
    }

    /// Sets the render scale of the 3D views.
    pub fn set_render_scale(&self, scale: f32) -> Result<(), Fault> {
        guest::write_f32(self.base + offset::RENDER_SCALE, scale)
    }

    /// Sets the growth of the render buffer pool and of the heaps around it.
    pub fn set_pool(&self, factor: u32, extra_bytes: u64) -> Result<(), Fault> {
        guest::write_u32(self.base + offset::POOL_FACTOR, factor)?;
        guest::write_u64(self.base + offset::EXTRA_MEMORY, extra_bytes)
    }

    /// Sets the output size of the game.
    pub fn set_output(&self, width: u32, height: u32) -> Result<(), Fault> {
        guest::write_u64(
            self.base + offset::OUTPUT_SIZE,
            (u64::from(height) << 32) | u64::from(width),
        )?;
        guest::write_f32(self.base + offset::OUTPUT_WIDTH, width as f32)
    }

    /// Sets the step of the next frame in whole vsyncs.
    pub fn set_step_vsyncs(&self, vsyncs: u32) -> Result<(), Fault> {
        guest::write_u32(self.base + offset::STEP_VSYNCS, vsyncs)
    }

    /// Sets the variable step of the next frame in units of 1/30 s, and the whole vsync step to 1.
    pub fn set_variable_step(&self, step: f32) -> Result<(), Fault> {
        guest::write_f32(self.base + offset::VARIABLE_STEP, step)?;
        self.set_step_vsyncs(1)
    }

    /// Clears the variable step, so the whole vsync step applies.
    pub fn clear_variable_step(&self) -> Result<(), Fault> {
        guest::write_f32(self.base + offset::VARIABLE_STEP, 0.0)
    }

    /// The game's own override of the step. Nonzero while the game steps by itself.
    pub fn own_step(&self) -> Result<u32, Fault> {
        guest::read_u32(self.base + offset::OWN_STEP)
    }

    /// The vsyncs that the game has stepped.
    pub fn stepped(&self) -> Result<u64, Fault> {
        guest::read_u64(self.base + offset::STEPPED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_grows_by_the_square_of_the_scale_rounded_up() {
        assert_eq!(pool_factor(1.0), 1);
        assert_eq!(pool_factor(1.2), 2);
        assert_eq!(pool_factor(1.6), 3);
        assert_eq!(pool_factor(2.4), 6);
        assert_eq!(pool_factor(3.0), 9);
    }

    #[test]
    fn the_extra_memory_is_the_growth_and_64_mb_in_2_mb_steps() {
        let pool = 0x730_0000;
        assert_eq!(extra_memory(pool, 1), 64 << 20);
        let x = extra_memory(pool, 3);
        assert_eq!(x % (2 << 20), 0);
        assert!(x >= 2 * pool + (64 << 20));
        assert!(x < 2 * pool + (64 << 20) + (2 << 20));
    }

    #[test]
    fn the_output_grows_only_when_the_views_outgrow_it() {
        assert_eq!(output_for(1.0, (1600, 900), 1920), None);
        assert_eq!(output_for(1.2, (1600, 900), 1920), None);
        assert_eq!(output_for(1.6, (1600, 900), 1920), Some((2560, 1440)));
        assert_eq!(output_for(2.4, (1600, 900), 1920), Some((3840, 2160)));
    }

    #[test]
    fn the_fields_land_at_their_offsets() {
        let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut page = vec![0u64; 0x60 / 8 + 2];
        let base = page.as_mut_ptr() as u64;
        guest::set_range(base, (page.len() * 8) as u64);
        let Ok(p) = StatePage::new(base) else {
            return;
        };
        assert!(p.set_target_fps(120.0).is_ok());
        assert!(p.set_render_scale(1.6).is_ok());
        assert!(p.set_pool(3, 0x1000).is_ok());
        assert!(p.set_output(2560, 1440).is_ok());
        assert!(p.set_variable_step(0.25).is_ok());
        assert_eq!(guest::read_f32(base).ok(), Some(120.0));
        assert_eq!(guest::read_f32(base + 0x10).ok(), Some(1.6));
        assert_eq!(guest::read_u32(base + 0x4c).ok(), Some(3));
        assert_eq!(guest::read_u64(base + 0x58).ok(), Some(0x1000));
        assert_eq!(guest::read_u64(base + 0x40).ok(), Some((1440 << 32) | 2560));
        assert_eq!(guest::read_f32(base + 0x48).ok(), Some(2560.0));
        assert_eq!(guest::read_f32(base + 0x50).ok(), Some(0.25));
        assert_eq!(guest::read_u32(base + 0x20).ok(), Some(1));
        assert!(p.clear_variable_step().is_ok());
        assert_eq!(guest::read_f32(base + 0x50).ok(), Some(0.0));
        let end = base + (page.len() * 8) as u64;
        assert!(StatePage::new(end).is_err());
    }
}
