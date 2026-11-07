//! The render scale policy (R28).
//!
//! The host renders the game at a scale of the game's own output size. This module picks the scale from the size of
//! the drawable and the size the game outputs. It only returns a number. The encoder applies it to the viewport and
//! the render targets, and the title module tells the game through its own channel (for Breath of the Wild, the state
//! page of `nxkit-svc-pacing`).
//!
//! The limits come from the loader of nxnative (`dm/host/loader.c`, pull request 76, read 2026-11-07). A scale above
//! 1.0 needs memory for a larger render pool in the game, and nxnative caps the scale at 3.0.

use crate::error::{MetalError, Result};

/// The smallest render scale.
pub const MIN_SCALE: f32 = 0.5;

/// The largest render scale.
pub const MAX_SCALE: f32 = 3.0;

/// Scales are multiples of this step, so the sizes of render targets take few distinct values.
pub const SCALE_STEP: f32 = 0.25;

/// With MetalFX, the render size is the drawable size divided by this ratio, at most.
///
/// The value is a tuning choice. No measurement on a real game backs it yet.
pub const METALFX_RATIO: f32 = 1.5;

/// The factor between the size the game outputs and the size the host renders.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderScale(f32);

impl RenderScale {
    /// The game's own size.
    pub const NATIVE: RenderScale = RenderScale(1.0);

    /// A scale from `MIN_SCALE` to `MAX_SCALE`. It returns `None` outside that range and for a value that is not finite.
    pub fn new(factor: f32) -> Option<Self> {
        (factor.is_finite() && (MIN_SCALE..=MAX_SCALE).contains(&factor)).then_some(Self(factor))
    }

    /// The factor.
    pub fn factor(self) -> f32 {
        self.0
    }

    /// The size of a render target for a game size. Each side is rounded up to an even number.
    pub fn apply(self, width: u32, height: u32) -> (u32, u32) {
        (scale_side(width, self.0), scale_side(height, self.0))
    }

    /// The factor of the render pool of the game, which is the scale squared and rounded up.
    ///
    /// A scale of 1.0 or less needs no extra pool, so the factor is 1.
    pub fn pool_factor(self) -> u32 {
        let sq = f64::from(self.0) * f64::from(self.0);
        // A scale of at most 3.0 gives at most 9, so the cast is exact.
        (sq - 1e-6).ceil().max(1.0) as u32
    }
}

fn scale_side(side: u32, factor: f32) -> u32 {
    let v = (f64::from(side) * f64::from(factor) - 1e-6).ceil().max(2.0) as u64;
    let even = v + (v & 1);
    u32::try_from(even).unwrap_or(u32::MAX & !1)
}

/// How the game's own upscaler is treated. It mirrors `nxtitle::Upscaler`, which this crate does not depend on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpscaleMode {
    /// The game's own pass runs.
    Game,
    /// The game renders at the output size and its upscaler is skipped.
    Native,
    /// MetalFX replaces the game's pass.
    MetalFx,
}

/// What the scale depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScaleInput {
    /// The size of the drawable in pixels.
    pub drawable: (u32, u32),
    /// The size the game outputs, for example 1280 by 720 or 1920 by 1080.
    pub game: (u32, u32),
    /// The treatment of the game's upscaler.
    pub mode: UpscaleMode,
}

/// The result of the policy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScaleChoice {
    /// The scale to apply.
    pub scale: RenderScale,
    /// The size to render at, which is `scale.apply(game)`.
    pub render: (u32, u32),
    /// True when the render size differs from the drawable size, so a spatial upscaler has work to do.
    pub needs_upscale: bool,
    /// Why this scale.
    pub reason: &'static str,
}

fn floor_step(v: f32) -> f32 {
    ((v + 1e-4) / SCALE_STEP).floor() * SCALE_STEP
}

fn clamp_scale(v: f32) -> RenderScale {
    RenderScale(v.clamp(MIN_SCALE, MAX_SCALE))
}

/// Picks the render scale.
///
/// Without MetalFX the host renders at the largest step that fits in the drawable on both sides. With MetalFX the
/// host renders at [`METALFX_RATIO`] times less per side and the scaler makes the rest, but never below the game's own
/// size unless the drawable is smaller than that.
pub fn choose_render_scale(input: &ScaleInput) -> Result<ScaleChoice> {
    let (dw, dh) = input.drawable;
    let (gw, gh) = input.game;
    if dw == 0 || dh == 0 || gw == 0 || gh == 0 {
        return Err(MetalError::Invalid("a size of the render scale is zero"));
    }
    let fit = (dw as f32 / gw as f32).min(dh as f32 / gh as f32);
    let (scale, reason) = match input.mode {
        UpscaleMode::Game | UpscaleMode::Native => (
            clamp_scale(floor_step(fit)),
            "the largest step that fits in the drawable",
        ),
        UpscaleMode::MetalFx => {
            let scaled = floor_step(fit / METALFX_RATIO);
            let own = floor_step(fit).min(1.0);
            (
                clamp_scale(scaled.max(own)),
                "MetalFX renders below the drawable size and the scaler fills the rest",
            )
        }
    };
    let render = scale.apply(gw, gh);
    Ok(ScaleChoice {
        scale,
        render,
        needs_upscale: input.mode == UpscaleMode::MetalFx && render != input.drawable,
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(drawable: (u32, u32), game: (u32, u32), mode: UpscaleMode) -> ScaleInput {
        ScaleInput {
            drawable,
            game,
            mode,
        }
    }

    #[test]
    fn range_is_checked() {
        assert!(RenderScale::new(0.49).is_none());
        assert!(RenderScale::new(3.01).is_none());
        assert!(RenderScale::new(f32::NAN).is_none());
        assert_eq!(RenderScale::new(1.5).map(RenderScale::factor), Some(1.5));
    }

    #[test]
    fn sizes_round_up_to_even() {
        let s = RenderScale(1.25);
        assert_eq!(s.apply(1280, 720), (1600, 900));
        assert_eq!(RenderScale(0.75).apply(1281, 721), (962, 542));
        assert_eq!(RenderScale::NATIVE.apply(1, 1), (2, 2));
    }

    #[test]
    fn pool_factor_is_the_square_rounded_up() {
        assert_eq!(RenderScale(1.0).pool_factor(), 1);
        assert_eq!(RenderScale(0.5).pool_factor(), 1);
        assert_eq!(RenderScale(1.25).pool_factor(), 2);
        assert_eq!(RenderScale(2.0).pool_factor(), 4);
        assert_eq!(RenderScale(3.0).pool_factor(), 9);
    }

    #[test]
    fn four_k_from_720p_native_renders_three_times() {
        let c = choose_render_scale(&input((3840, 2160), (1280, 720), UpscaleMode::Native));
        let c = c.unwrap_or_else(|_| unreachable!());
        assert_eq!(c.scale.factor(), 3.0);
        assert_eq!(c.render, (3840, 2160));
        assert!(!c.needs_upscale);
    }

    #[test]
    fn four_k_from_1080p_with_metalfx_renders_below_the_drawable() {
        let c = choose_render_scale(&input((3840, 2160), (1920, 1080), UpscaleMode::MetalFx));
        let c = c.unwrap_or_else(|_| unreachable!());
        assert_eq!(c.scale.factor(), 1.25);
        assert_eq!(c.render, (2400, 1350));
        assert!(c.needs_upscale);
    }

    #[test]
    fn a_small_drawable_scales_down_and_never_below_the_minimum() {
        let c = choose_render_scale(&input((640, 360), (1920, 1080), UpscaleMode::Game));
        let c = c.unwrap_or_else(|_| unreachable!());
        assert_eq!(c.scale.factor(), MIN_SCALE);
    }

    #[test]
    fn zero_sizes_are_refused() {
        assert!(choose_render_scale(&input((0, 1), (1, 1), UpscaleMode::Game)).is_err());
    }
}
