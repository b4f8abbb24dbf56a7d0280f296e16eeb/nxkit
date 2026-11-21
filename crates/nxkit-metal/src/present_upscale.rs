//! Upscaling in the present (R70), as `DM_PRESENT_UPSCALE` of the old engine.
//!
//! The present draws the game's picture over the whole drawable. With [`PresentUpscale::MetalFx`] a picture that is
//! smaller than the drawable goes through MetalFX's spatial scaler first, and the present draws the scaled picture
//! one to one. Off, the present reads the picture by nearest pixel, as before.
//!
//! [`PresentUpscaler::encode`] copies the shown rectangle to the origin of a texture of its own, because MetalFX reads
//! from the origin. A compute pass does the copy. It waits for the earlier work of the queue and holds the later work
//! of the queue behind it, so the scaler's own encoders run after the game's frame and before the present pass. The
//! scaler gets the format of the picture. An sRGB picture is scaled in linear mode, every other one in perceptual mode,
//! since its values are what the display shows.
//!
//! A new scaler is made when the picture size, its format or the drawable size changes, at most once in
//! [`REMAKE_PRESENTS`] presents, so a window resize does not make one at every present. A size that MetalFX refuses
//! is drawn by nearest pixel until the size changes. The host passes the mode in. This module reads no environment.

use std::time::Instant;

use crate::barrier::{Dependency, Scope, Stages, Visibility};
use crate::compute::ComputeCommand;
use crate::device::Context;
use crate::encode::CommandBuffer;
use crate::format::Format;
use crate::table::ArgumentTable;
use crate::texture::{Storage, Texture, TextureDesc, TextureKind, TextureRegion};
use crate::upscale::{SpatialUpscaler, UpscaleDesc};

/// How the present scales a picture that is smaller than the drawable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PresentUpscale {
    /// By nearest pixel in the present pass.
    #[default]
    Off,
    /// Through MetalFX's spatial scaler, where the device has it.
    MetalFx,
}

impl PresentUpscale {
    /// The name in the log: `off` or `metalfx`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::MetalFx => "metalfx",
        }
    }
}

/// The presents between two new scalers.
pub const REMAKE_PRESENTS: u64 = 30;

/// True when a picture of `content` pixels is shown larger than it is: at least as large on both sides and larger
/// on one.
fn wanted(content: (u32, u32), output: (u32, u32)) -> bool {
    let (cw, ch) = content;
    let (ow, oh) = output;
    cw > 0 && ch > 0 && ow >= cw && oh >= ch && (ow > cw || oh > ch)
}

/// What a scaler is made for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    content: (u32, u32),
    format: Format,
    output: (u32, u32),
}

/// What the present does with a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// The scaler fits.
    Use,
    /// A new scaler is made.
    Make,
    /// Nearest pixel for this present: the key was refused, or a scaler was made too recently.
    Skip,
}

/// The scaler that `current` names, the key MetalFX refused last, the present of the last new scaler and the
/// present now decide the step for `key`.
fn step(
    current: Option<Key>,
    refused: Option<Key>,
    made_at: Option<u64>,
    now: u64,
    key: Key,
) -> Step {
    if current == Some(key) {
        Step::Use
    } else if refused == Some(key) || made_at.is_some_and(|at| now < at + REMAKE_PRESENTS) {
        Step::Skip
    } else {
        Step::Make
    }
}

struct Scaler {
    key: Key,
    fx: SpatialUpscaler,
    input: Texture,
    output: Texture,
}

/// The upscaler of one present path. Only the thread that presents uses it.
pub struct PresentUpscaler {
    mode: PresentUpscale,
    scaler: Option<Scaler>,
    refused: Option<Key>,
    made_at: Option<u64>,
    made: u32,
    presents: u64,
    table: Option<ArgumentTable>,
}

impl PresentUpscaler {
    /// An upscaler in `mode`. With MetalFX, a device without the Metal 4 spatial scaler gets `Off` and a log line.
    pub fn new(ctx: &Context, mode: PresentUpscale) -> Self {
        let mode = if mode == PresentUpscale::MetalFx && !SpatialUpscaler::supported(ctx) {
            eprintln!(
                "nxkit-metal: present upscaling: this GPU has no Metal 4 MetalFX spatial scaler, the present scales by nearest pixel"
            );
            PresentUpscale::Off
        } else {
            mode
        };
        eprintln!("nxkit-metal: present upscaling: {}", mode.name());
        Self {
            mode,
            scaler: None,
            refused: None,
            made_at: None,
            made: 0,
            presents: 0,
            table: None,
        }
    }

    /// The mode in effect.
    pub fn mode(&self) -> PresentUpscale {
        self.mode
    }

    /// Encodes the scaling of the rectangle `rect` (x, y, width, height) of `src` to `output` pixels into `cmd`.
    /// Returns the scaled picture, which the present draws whole and one to one. None leaves the picture to the
    /// present as it is: the mode is off, the picture is not shown larger, or the scaler cannot take it.
    ///
    /// No pass may be open on the command buffer.
    pub fn encode(
        &mut self,
        ctx: &Context,
        cmd: &mut CommandBuffer,
        src: &Texture,
        rect: [u32; 4],
        output: (u32, u32),
    ) -> Option<Texture> {
        self.presents += 1;
        let content = (rect[2], rect[3]);
        if self.mode == PresentUpscale::Off || !wanted(content, output) {
            return None;
        }
        let key = Key {
            content,
            format: src.desc().format,
            output,
        };
        let current = self.scaler.as_ref().map(|s| s.key);
        match step(current, self.refused, self.made_at, self.presents, key) {
            Step::Use => {}
            Step::Skip => return None,
            Step::Make => self.make(ctx, key)?,
        }
        if let Err(e) = self.run(ctx, cmd, src, rect) {
            eprintln!("nxkit-metal: present upscaling: {e}, this size is scaled by nearest pixel");
            self.refused = Some(key);
            self.scaler = None;
            return None;
        }
        self.scaler.as_ref().map(|s| s.output.clone())
    }

    fn make(&mut self, ctx: &Context, key: Key) -> Option<()> {
        self.scaler = None;
        self.made_at = Some(self.presents);
        self.made += 1;
        let t0 = Instant::now();
        let linear = matches!(key.format, Format::Rgba8UnormSrgb | Format::Bgra8UnormSrgb);
        let made = (|| {
            let fx = SpatialUpscaler::new(
                ctx,
                UpscaleDesc {
                    input: key.content,
                    output: key.output,
                    input_format: key.format,
                    output_format: key.format,
                    linear,
                },
            )?;
            let input = Texture::new(ctx, own_texture(key.content, key.format, false))?;
            let output = Texture::new(ctx, own_texture(key.output, key.format, true))?;
            crate::error::Result::Ok(Scaler {
                key,
                fx,
                input,
                output,
            })
        })();
        match made {
            Ok(s) => {
                eprintln!(
                    "nxkit-metal: present upscaling: MetalFX spatial scaler {}x{} -> {}x{} {:?} (scaler #{}, made in {:.1} ms)",
                    key.content.0,
                    key.content.1,
                    key.output.0,
                    key.output.1,
                    key.format,
                    self.made,
                    t0.elapsed().as_secs_f64() * 1e3
                );
                self.scaler = Some(s);
                Some(())
            }
            Err(e) => {
                eprintln!(
                    "nxkit-metal: present upscaling: no MetalFX spatial scaler for {}x{} -> {}x{} {:?}: {e}, this size is scaled by nearest pixel",
                    key.content.0, key.content.1, key.output.0, key.output.1, key.format
                );
                self.refused = Some(key);
                None
            }
        }
    }

    fn run(
        &mut self,
        ctx: &Context,
        cmd: &mut CommandBuffer,
        src: &Texture,
        rect: [u32; 4],
    ) -> crate::error::Result<()> {
        let Some(s) = self.scaler.as_ref() else {
            return Ok(());
        };
        if self.table.is_none() {
            self.table = Some(ArgumentTable::new(ctx, 1, 1)?);
        }
        let Some(table) = self.table.as_ref() else {
            return Ok(());
        };
        {
            let mut pass = cmd.begin_compute_pass(table)?;
            pass.encode(&ComputeCommand::Depend(Dependency {
                scope: Scope::FromEarlierPasses,
                after: Stages::ALL,
                before: Stages::BLIT,
                visibility: Visibility::Device,
            }))?;
            pass.encode(&ComputeCommand::CopyTexture {
                src: src.clone(),
                dst: s.input.clone(),
                src_region: TextureRegion {
                    level: 0,
                    slice: 0,
                    origin: [rect[0], rect[1], 0],
                    size: [rect[2], rect[3], 1],
                },
                dst_level: 0,
                dst_slice: 0,
                dst_origin: [0; 3],
            })?;
            pass.encode(&ComputeCommand::Depend(Dependency {
                scope: Scope::ToLaterPasses,
                after: Stages::BLIT,
                before: Stages::ALL,
                visibility: Visibility::Device,
            }))?;
            pass.end();
        }
        cmd.retain(src.clone());
        s.fx.encode(cmd, &s.input, s.key.content, &s.output)
    }
}

/// A private 2D texture of the scaler: its input (shader read) or its output (shader read and write).
fn own_texture(size: (u32, u32), format: Format, output: bool) -> TextureDesc {
    TextureDesc {
        kind: TextureKind::D2,
        width: size.0,
        height: size.1,
        depth: 1,
        mip_levels: 1,
        array_layers: 1,
        allow_views: false,
        format,
        shader_read: true,
        shader_write: output,
        render_target: false,
        storage: Storage::Private,
        shader_atomic: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(w: u32) -> Key {
        Key {
            content: (w, 1080),
            format: Format::Rgba8Unorm,
            output: (3840, 2160),
        }
    }

    #[test]
    fn only_a_picture_shown_larger_is_scaled() {
        assert!(wanted((1920, 1080), (3840, 2160)));
        assert!(wanted((1920, 1080), (1920, 1200)), "larger on one side");
        assert!(!wanted((1920, 1080), (1920, 1080)), "the same size");
        assert!(!wanted((1920, 1080), (2560, 1000)), "smaller on one side");
        assert!(!wanted((1920, 1080), (1280, 720)), "smaller");
        assert!(!wanted((0, 1080), (3840, 2160)), "no picture");
    }

    #[test]
    fn a_new_scaler_waits_for_the_remake_interval() {
        assert_eq!(step(None, None, None, 1, key(1920)), Step::Make);
        assert_eq!(
            step(Some(key(1920)), None, Some(1), 2, key(1920)),
            Step::Use
        );
        assert_eq!(
            step(Some(key(1920)), None, Some(1), 30, key(1600)),
            Step::Skip,
            "a resize right after a new scaler"
        );
        assert_eq!(
            step(
                Some(key(1920)),
                None,
                Some(1),
                1 + REMAKE_PRESENTS,
                key(1600)
            ),
            Step::Make
        );
    }

    #[test]
    fn a_refused_size_stays_refused_until_it_changes() {
        assert_eq!(step(None, Some(key(1920)), None, 99, key(1920)), Step::Skip);
        assert_eq!(step(None, Some(key(1920)), None, 99, key(1600)), Step::Make);
    }

    fn cleared(
        ctx: &Context,
        cmd: &mut CommandBuffer,
        size: (u32, u32),
        rgba: [f64; 4],
    ) -> Texture {
        use crate::render::{ColorAttachment, RenderPassDesc};
        use crate::{LoadAction, StoreAction};
        let mut d = own_texture(size, Format::Rgba8Unorm, false);
        d.render_target = true;
        let t = Texture::new(ctx, d).unwrap();
        let table = ArgumentTable::new(ctx, 1, 1).unwrap();
        let desc = RenderPassDesc {
            colors: vec![Some(ColorAttachment {
                depth_plane: 0,
                texture: t.clone(),
                load: LoadAction::Clear,
                store: StoreAction::Store,
                clear: rgba,
                memoryless: false,
            })],
            depth: None,
            stencil: None,
        };
        cmd.begin_render_pass(&desc, &table).unwrap().end();
        t
    }

    #[test]
    fn the_shown_rectangle_is_scaled_to_the_output() {
        let Ok(ctx) = Context::system_default() else {
            eprintln!("skipped: no Metal device");
            return;
        };
        let mut ring = crate::FrameRing::new(&ctx).unwrap();
        let mut frame = ring.begin().unwrap();
        let cmd = frame.commands();
        // A green 32x24 picture at 8,4 of a red 64x48 window texture.
        let src = cleared(&ctx, cmd, (64, 48), [1.0, 0.0, 0.0, 1.0]);
        let green = cleared(&ctx, cmd, (32, 24), [0.0, 1.0, 0.0, 1.0]);
        let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
        let mut pass = cmd.begin_compute_pass(&table).unwrap();
        for c in [
            ComputeCommand::Depend(Dependency {
                scope: Scope::FromEarlierPasses,
                after: Stages::ALL,
                before: Stages::BLIT,
                visibility: Visibility::Device,
            }),
            ComputeCommand::CopyTexture {
                src: green.clone(),
                dst: src.clone(),
                src_region: TextureRegion::whole(0, 0, [32, 24, 1]),
                dst_level: 0,
                dst_slice: 0,
                dst_origin: [8, 4, 0],
            },
        ] {
            pass.encode(&c).unwrap();
        }
        pass.end();
        let mut off = PresentUpscaler::new(&ctx, PresentUpscale::Off);
        assert!(
            off.encode(&ctx, cmd, &src, [8, 4, 32, 24], (64, 48))
                .is_none()
        );
        let mut fx = PresentUpscaler::new(&ctx, PresentUpscale::MetalFx);
        assert!(
            fx.encode(&ctx, cmd, &src, [8, 4, 32, 24], (32, 24))
                .is_none(),
            "not shown larger"
        );
        if fx.mode() == PresentUpscale::Off {
            eprintln!("skipped: no MetalFX spatial scaler");
            return;
        }
        let out = fx
            .encode(&ctx, cmd, &src, [8, 4, 32, 24], (64, 48))
            .unwrap();
        assert_eq!((out.desc().width, out.desc().height), (64, 48));
        frame.submit().unwrap().wait().unwrap();
        let pts = [[0, 0], [63, 0], [0, 47], [63, 47], [32, 24]];
        for (p, px) in pts.iter().zip(out.debug_read(&ctx, &pts).unwrap()) {
            assert!(px[1] > 200 && px[0] < 40 && px[2] < 40, "{p:?}: {px:?}");
        }
    }
}
