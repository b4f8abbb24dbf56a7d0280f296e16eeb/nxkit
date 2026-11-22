//! Temporal upscaling in place of a game's own upscale pass (R72, `temporal_upscale` of `game.json`).
//!
//! The title names three kinds of draws by their fragment programs. The pass-through draws (the game's temporal
//! anti-aliasing) are left out, and a copy of one of their textures (the jittered scene) goes into colour attachment 0
//! of their pass. The upscale draw is left out too. Before its pass a compute pass writes the motion of each pixel,
//! MetalFX's temporal scaler accumulates the jittered picture at the output size, and a copy draw writes it into colour
//! attachment 0 of the pass. The pass then loads that attachment, and the game's later draws of the pass (its UI) draw
//! on top. The source draw gives the depth, the velocity and the view uniform buffer with the matrix and the jitter.
//!
//! [`Tupscale::prepare`] decides once per frame, on the thread that encodes the frame, before any pass is encoded: it
//! finds the draws, resolves every texture, reads the view values and makes the scaler. When an input is missing or
//! does not fit, nothing is replaced and the game's own draws run, so a frame never gets half of it. The log names the
//! reason once (`nxkit-gpu: temporal upscale: ...`). [`Prepared::before_pass`] runs on the encode threads, in the pass
//! of each replaced draw. Each replaced draw is the first draw of its pass, so the replacement goes before the pass.
//!
//! The scaler, its own textures, the pipelines and the counters live in [`Tupscale`] behind a mutex. MetalFX keeps the
//! history in the scaler. A new scaler, a camera cut, or a frame after a frame without a run of the scaler resets it.

mod find;
mod make;
mod math;
mod msl;
mod run;
#[cfg(test)]
mod tests;

use find::{bound, color0, render_passes, srgb, view_bytes};
pub(crate) use run::Prepared;
use run::{PassCopy, Upscale};

use std::collections::{BTreeMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use nxkit_metal::{
    ArchiveConfig, ArgumentTable, ColorAttachment, CommandBuffer, ComputeCommand, ComputePipeline,
    Context, Dependency, Format, LoadAction, MetalError, PipelineCache, Primitive, RenderCommand,
    RenderPassDesc, RenderPipeline, RenderPipelineDesc, Scope, Scratch, ShaderLibrary, Stages,
    Storage, StoreAction, TemporalDesc, TemporalFrame, TemporalUpscaler, Texture, TextureDesc,
    TextureKind, Visibility,
};
use nxkit_nvn::frame::Cmd;
use nxkit_nvn::passes::{PassKind, Slot};
use nxkit_nvn::resource::TexId;

use crate::detail::{DrawDetail, Stage};
use crate::drawctx::Shared;
use crate::settings::{TemporalUpscale, UboSlot};
use crate::textures::Res;
use math::Signs;

/// The barrier that a pass with a replaced first draw starts with: it loads what the copy draw wrote.
pub(crate) const LOAD_AFTER_COPY: Dependency = Dependency {
    scope: Scope::FromEarlierPasses,
    after: Stages::FRAGMENT,
    before: Stages::FRAGMENT,
    visibility: Visibility::Device,
};

/// The format of the motion texture.
const MOTION_FORMAT: Format = Format::Rg16Float;

/// The frames between two new scalers. A scaler takes about a second to make.
const REMAKE_FRAMES: u64 = 30;

/// The frames between two lines of the `tupscale` diagnostic.
const DIAG_FRAMES: u64 = 120;

/// What a scaler is made for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    input: (u32, u32),
    output: (u32, u32),
    color: Format,
    depth: Format,
}

struct Scaler {
    key: Key,
    fx: Arc<TemporalUpscaler>,
    motion: Texture,
    output: Texture,
}

/// The counters of the frames since the last diagnostic line, and the values of the last frame that ran.
#[derive(Default)]
struct Window {
    ran: u32,
    absent: u32,
    resets: u32,
    fell: BTreeMap<String, u32>,
    last: Option<Last>,
}

/// The view values of the last frame that ran, for the diagnostic.
#[derive(Clone, Copy)]
struct Last {
    m: math::Rows,
    jitter: [f32; 2],
    pixels: (f32, f32),
    row_sign: f32,
    cut: bool,
}

#[derive(Default)]
struct State {
    frame: u64,
    scaler: Option<Scaler>,
    refused: Option<Key>,
    made_at: Option<u64>,
    made: u32,
    last_run: Option<u64>,
    logged: HashSet<String>,
    configs: HashSet<u64>,
    lib: Option<ShaderLibrary>,
    motion: Option<ComputePipeline>,
    copies: Vec<(Format, RenderPipeline)>,
    window: Window,
}

impl State {
    /// Counts a frame that runs the game's draws for `why`, and logs `why` the first time.
    fn fall(&mut self, why: String) {
        self.last_run = None;
        if self.logged.insert(why.clone()) {
            eprintln!("nxkit-gpu: temporal upscale: {why}, the game's draws run");
        }
        *self.window.fell.entry(why).or_default() += 1;
    }
}

/// Why a frame keeps the game's draws.
enum Miss {
    /// The frame has no upscale draw (a menu or a loading screen). Not logged.
    Absent,
    /// An input is missing or does not fit.
    Why(String),
}

fn why(s: impl Into<String>) -> Miss {
    Miss::Why(s.into())
}

/// The temporal upscale of the encoder, made when `game.json` names one.
pub(crate) struct Tupscale {
    cfg: TemporalUpscale,
    ctx: Context,
    cache: PipelineCache,
    state: Arc<Mutex<State>>,
}

impl Tupscale {
    /// The temporal upscale of `cfg`. None, with a log line, when `NXKIT_TEMPORAL_UPSCALE=0` turns it off or the
    /// device has no MetalFX temporal scaler for Metal 4.
    pub(crate) fn new(ctx: &Context, cfg: Option<TemporalUpscale>) -> Option<Self> {
        let cfg = cfg?;
        let off = |why: &str| eprintln!("nxkit-gpu: temporal upscale: off, {why}");
        if !crate::settings::env().temporal_upscale {
            off("NXKIT_TEMPORAL_UPSCALE=0");
            return None;
        }
        if !TemporalUpscaler::supported(ctx) {
            off("this GPU has no Metal 4 MetalFX temporal scaler");
            return None;
        }
        let cache = match PipelineCache::new(ctx, &ArchiveConfig::default()) {
            Ok(c) => c,
            Err(e) => {
                off(&format!("no pipeline cache: {e}"));
                return None;
            }
        };
        eprintln!(
            "nxkit-gpu: temporal upscale: on, upscale program {} slot {}, source program {}, {} pass-through program(s)",
            cfg.upscale.fragment,
            cfg.upscale.slot,
            cfg.source.fragment,
            cfg.passthrough.len()
        );
        Some(Self {
            cfg,
            ctx: ctx.clone(),
            cache,
            state: Arc::default(),
        })
    }

    /// Decides for the frame of `sh` whether the draws are replaced, and prepares the replacement.
    pub(crate) fn prepare(&self, sh: &Shared<'_>) -> Option<Prepared> {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.frame += 1;
        let out = match self.plan(sh, &mut st) {
            Ok(p) => {
                st.window.ran += 1;
                st.window.resets += u32::from(p.upscale.reset);
                st.last_run = Some(st.frame);
                Some(p)
            }
            Err(Miss::Absent) => {
                st.window.absent += 1;
                st.last_run = None;
                None
            }
            Err(Miss::Why(w)) => {
                st.fall(w);
                None
            }
        };
        if st.frame.is_multiple_of(DIAG_FRAMES) {
            if crate::diag("tupscale") {
                print_window(&st.window, st.frame);
            }
            st.window = Window::default();
        }
        out
    }

    fn plan(&self, sh: &Shared<'_>, st: &mut State) -> Result<Prepared, Miss> {
        let cfg = &self.cfg;
        let draws = &sh.detail.draws;
        let is = |d: &DrawDetail, key: &str| d.program.fragment.as_deref() == Some(key);
        let up = draws
            .iter()
            .position(|d| is(d, &cfg.upscale.fragment))
            .ok_or(Miss::Absent)?;
        let passes = render_passes(sh);
        let first_of = |n: usize, what: &str| -> Result<usize, Miss> {
            let (i, r) = passes
                .iter()
                .find(|(_, r)| r.contains(&(n as u32)))
                .ok_or_else(|| why(format!("the {what} draw is in no render pass")))?;
            if r.start != n as u32 {
                return Err(why(format!(
                    "the {what} draw is not the first draw of its render pass"
                )));
            }
            Ok(*i)
        };
        let up_d = &draws[up];
        let up_pass = first_of(up, "upscale")?;
        let (color_id, color) = bound(sh, up_d, cfg.upscale.slot, "upscale")?;
        let (target_id, target) = color0(sh, up_pass, "upscale")?;
        let src = draws[..up]
            .iter()
            .rposition(|d| is(d, &cfg.source.fragment))
            .ok_or_else(|| why("no draw of the source program before the upscale draw"))?;
        let src_d = &draws[src];
        let (depth_id, depth) = bound(sh, src_d, cfg.source.depth_slot, "depth")?;
        let (velocity_id, velocity) = bound(sh, src_d, cfg.source.velocity_slot, "velocity")?;
        let ubo = view_bytes(sh, src_d, cfg.source.view)?;
        let short = |at: u32| {
            why(format!(
                "the view uniform buffer of {} bytes ends before byte {at}",
                ubo.len()
            ))
        };
        let m = math::rows(&ubo, cfg.view.clip_to_prev_clip)
            .ok_or_else(|| short(cfg.view.clip_to_prev_clip))?;
        let jitter: [f32; 2] =
            math::floats(&ubo, cfg.view.jitter).ok_or_else(|| short(cfg.view.jitter))?;
        let view: [f32; 2] =
            math::floats(&ubo, cfg.view.view_size).ok_or_else(|| short(cfg.view.view_size))?;
        if !m
            .iter()
            .flatten()
            .chain(&jitter)
            .chain(&view)
            .all(|v| v.is_finite())
        {
            return Err(why("the view values are not finite"));
        }

        let cd = color.tex.desc();
        let input = [cd.width, cd.height];
        let scaled = view.map(|v| (v * color.scale).round() as u32);
        if scaled != input {
            return Err(why(format!(
                "the view is {}x{} pixels and the picture {}x{}",
                scaled[0], scaled[1], input[0], input[1]
            )));
        }
        let dd = depth.tex.desc();
        let vd = velocity.tex.desc();
        let td = target.desc();
        if !dd.format.is_depth() || dd.width < input[0] || dd.height < input[1] {
            return Err(why(format!(
                "the depth is {:?} {}x{}, smaller than the picture or not a depth format",
                dd.format, dd.width, dd.height
            )));
        }
        if vd.format.is_depth() || vd.width < input[0] || vd.height < input[1] || !vd.shader_read {
            return Err(why(format!(
                "the velocity is {:?} {}x{}, smaller than the picture or not readable",
                vd.format, vd.width, vd.height
            )));
        }
        let wanted = td.width >= input[0]
            && td.height >= input[1]
            && (td.width > input[0] || td.height > input[1]);
        if !wanted || srgb(td.format) != srgb(cd.format) {
            return Err(why(format!(
                "colour attachment 0 of the upscale pass is {:?} {}x{} for a picture of {:?} {}x{}",
                td.format, td.width, td.height, cd.format, input[0], input[1]
            )));
        }

        let mut copies = Vec::new();
        let mut shown = Vec::new();
        for (n, d) in draws[..up].iter().enumerate() {
            let Some(p) = cfg.passthrough.iter().find(|p| is(d, &p.fragment)) else {
                continue;
            };
            let pass = first_of(n, "pass-through")?;
            let (sid, s) = bound(sh, d, p.slot, "pass-through")?;
            let (tid, t) = color0(sh, pass, "pass-through")?;
            let (a, b) = (s.tex.desc(), t.desc());
            if (a.width, a.height) != (b.width, b.height) || !a.shader_read {
                return Err(why(format!(
                    "the pass-through texture is {:?} {}x{} and colour attachment 0 of its pass {:?} {}x{}",
                    a.format, a.width, a.height, b.format, b.width, b.height
                )));
            }
            shown.push(format!(
                "draw {n} {sid:?} {:?} {}x{} -> {tid:?} {:?}",
                a.format, a.width, a.height, b.format
            ));
            copies.push(PassCopy {
                draw: n as u32,
                src: s.tex.clone(),
                format: b.format,
                pipe: self.copy_pipeline(st, b.format)?,
            });
        }
        if copies.is_empty() && !cfg.passthrough.is_empty() {
            return Err(why("no pass-through draw before the upscale draw"));
        }

        let key = Key {
            input: (input[0], input[1]),
            output: (td.width, td.height),
            color: cd.format,
            depth: dd.format,
        };
        let new = self.scaler(st, key)?;
        let Some(scaler) = st.scaler.as_ref() else {
            return Err(why("no scaler"));
        };
        let (fx, motion, output) = (
            scaler.fx.clone(),
            scaler.motion.clone(),
            scaler.output.clone(),
        );
        let frame = TemporalFrame {
            color: &color.tex,
            depth: &depth.tex,
            motion: &motion,
            reactive: None,
            content: key.input,
            jitter: (0.0, 0.0),
            motion_scale: (1.0, 1.0),
            depth_reversed: cfg.depth_reversed,
            reset: false,
        };
        fx.accepts(&frame, &output)
            .map_err(|e| why(format!("MetalFX does not take the inputs: {e}")))?;
        let motion_pipe = self.motion_pipeline(st)?;
        let copy = self.copy_pipeline(st, td.format)?;

        // The rows of the source pass run up the screen when its viewport has a negative height.
        let row_sign = if src_d.dynamic.viewport[3] < 0.0 {
            -1.0
        } else {
            1.0
        };
        let signs = Signs::from_knobs();
        let px = math::jitter_pixels(jitter, input, row_sign, &signs);
        let cut = math::camera_cut(&m, cfg.depth_reversed);
        let reset = new || cut || st.last_run != Some(st.frame - 1);
        st.window.last = Some(Last {
            m,
            jitter,
            pixels: px,
            row_sign,
            cut,
        });
        if crate::diag("tupscale") {
            let line = format!(
                "colour {color_id:?} {:?} {}x{} depth {depth_id:?} {:?} {}x{} velocity {velocity_id:?} {:?} {}x{} target {target_id:?} {:?} {}x{} row_sign {row_sign} pass-through [{}]",
                cd.format,
                cd.width,
                cd.height,
                dd.format,
                dd.width,
                dd.height,
                vd.format,
                vd.width,
                vd.height,
                td.format,
                td.width,
                td.height,
                shown.join(", ")
            );
            let mut h = DefaultHasher::new();
            line.hash(&mut h);
            if st.configs.len() < 64 && st.configs.insert(h.finish()) {
                eprintln!("DIAG tupscale config {line}");
            }
        }
        Ok(Prepared {
            upscale: Upscale {
                draw: up as u32,
                color: color.tex.clone(),
                depth: depth.tex.clone(),
                velocity: velocity.tex.clone(),
                motion,
                output,
                fx,
                params: msl::params(&m, input, &cfg.velocity, &signs, row_sign),
                size: input,
                jitter: px,
                reset,
                depth_reversed: cfg.depth_reversed,
                motion_pipe,
                copy,
                format: td.format,
            },
            copies,
            state: self.state.clone(),
        })
    }
}

/// The line of the `tupscale` diagnostic for the frames up to `frame`.
fn print_window(w: &Window, frame: u64) {
    let fell: Vec<String> = w.fell.iter().map(|(k, n)| format!("{n}x {k}")).collect();
    let last = w.last.map_or_else(String::new, |l| {
        let (m, j, px) = (l.m, l.jitter, l.pixels);
        format!(
            " | last: jitter clip ({:.6}, {:.6}) pixels ({:.3}, {:.3}) row_sign {} clip_to_prev_clip off identity {:.5} row3 [{:.5}, {:.5}, {:.5}, {:.5}] cut {}",
            j[0],
            j[1],
            px.0,
            px.1,
            l.row_sign,
            math::off_identity(&m),
            m[3][0],
            m[3][1],
            m[3][2],
            m[3][3],
            l.cut
        )
    });
    eprintln!(
        "DIAG tupscale frame {frame}: ran {} resets {} absent {} fell back {} [{}]{last}",
        w.ran,
        w.resets,
        w.absent,
        w.fell.values().sum::<u32>(),
        fell.join("; ")
    );
}
