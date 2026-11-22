//! Live frame capture (W39). A person or an agent asks for one frame of the running game and gets files: the picture
//! that the frame showed, every render layer after its pass, and a manifest with the facts of each pass.
//!
//! One request waits in a queue. The encoder takes it at the start of the next frame (`begin`). The render pass hook
//! (`pass`, installed through `probe`) records the passes of that frame and copies their attachments on the GPU right
//! after each pass. When the frame has completed, `finish` reads the copies back and hands the bytes to the caller
//! through a channel. The caller converts them to pictures and writes the files on its own thread (`write`), so the
//! encoder waits only for the read back. A frame with no request costs one atomic load at its start and one for each
//! render pass. This module knows no socket: `nxkit-host` registers the verbs of the control socket over [`request`].
//!
//! - [`Want::Frame`]: the final picture, the pass list and the manifest.
//! - [`Want::Passes`]: also a picture of each attachment after its pass.
//! - [`Want::List`]: only the pass list, as text lines.
//! - [`Want::Texture`]: one texture by id as the frame left it.
//!
//! [`gpu_trace`] captures the next frames for Xcode. [`stats_lines`] reads the numbers of the last frames, and [`sample_next`] makes the next frame a sampled frame.

mod convert;
mod data;
mod jsonw;
mod manifest;
mod pass;
mod png;
mod stats;
mod trace;
mod write;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use nxkit_metal::{CommandBuffer, Context, Format, RenderPassDesc, Texture};
use nxkit_nvn::frame::{Cmd, Frame};
use nxkit_nvn::passes::{Pass, PassPlan};
use nxkit_nvn::resource::TexId;

use crate::drawctx::Shared;
use crate::probe::PassWatch;
use crate::stats::EncodeStats;
use data::{Captured, FrameInfo, Image, Key, PassRec};

pub(crate) use stats::note_frame;
pub use trace::{TraceTicket, gpu_trace};
pub(crate) use trace::{trace_begin, trace_end};

/// What a request asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Want {
    /// The presented picture and the manifest.
    Frame,
    /// [`Want::Frame`] and a picture of each attachment after its pass.
    Passes,
    /// The pass list as text only. No file is written.
    List,
    /// One texture by id (`TexId`).
    Texture(u32),
}

/// A request for the next frame.
#[derive(Clone, Debug)]
pub struct Request {
    /// What to capture.
    pub want: Want,
    /// The folder that gets `capture-<n>`. Without one, the diagnostics folder (`NXKIT_DIAG_DIR`).
    pub dir: Option<PathBuf>,
}

type Reply = Result<Captured, String>;

struct Job {
    want: Want,
    tx: Sender<Reply>,
}

/// A copy of an attachment that a pass made on the GPU.
pub(crate) struct Snap {
    pub(crate) pass: usize,
    pub(crate) label: String,
    pub(crate) tex: u32,
    pub(crate) format: Format,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) snapshot: Texture,
}

struct Run {
    job: Job,
    frame: u64,
    passes: Vec<PassRec>,
    snaps: Vec<Snap>,
    bytes: u64,
}

#[derive(Default)]
struct State {
    job: Option<Job>,
    run: Option<Run>,
}

/// The one queue of the capture service, written by the control thread and read by the encoder. The encoder reaches it
/// through the hooks of `probe`, which carry no context (W33, the opt-in analyses). Owning it in `Gpu` would need a
/// handle in the host, which should not know the encoder.
static STATE: Mutex<State> = Mutex::new(State {
    job: None,
    run: None,
});

/// The bits of [`FLAGS`].
const PENDING: u8 = 1;
const RUNNING: u8 = 2;
const HOOKS: u8 = 4;
/// A GPU trace waits or runs (`trace`).
const TRACE: u8 = 8;

/// What the encoder checks without a lock: a request waits, a frame is captured, the render pass hook records.
static FLAGS: AtomicU8 = AtomicU8::new(0);

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn set(bits: u8, on: bool) {
    if on {
        FLAGS.fetch_or(bits, Ordering::AcqRel);
    } else {
        FLAGS.fetch_and(!bits, Ordering::AcqRel);
    }
}

/// True while the render pass hook records.
fn hooks_on() -> bool {
    FLAGS.load(Ordering::Relaxed) & HOOKS != 0
}

/// True when the capture takes a picture of each attachment.
fn wants_pictures() -> bool {
    state()
        .run
        .as_ref()
        .is_some_and(|r| r.job.want == Want::Passes)
}

fn record(rec: PassRec) {
    if let Some(run) = state().run.as_mut() {
        run.passes.push(rec);
    }
}

fn keep(snap: Snap) {
    if let Some(run) = state().run.as_mut() {
        run.snaps.push(snap);
    }
}

/// Counts `bytes` of snapshots. False when the budget of the capture is used.
fn reserve(bytes: u64) -> bool {
    let mut st = state();
    let Some(run) = st.run.as_mut() else {
        return false;
    };
    if run.bytes + bytes > pass::BUDGET {
        return false;
    }
    run.bytes += bytes;
    true
}

/// Notes that an attachment of a pass got no copy.
fn skip(pass: usize, label: &str, why: &str) {
    if let Some(run) = state().run.as_mut()
        && let Some(a) = run
            .passes
            .iter_mut()
            .filter(|p| p.index == pass)
            .flat_map(|p| p.attachments.iter_mut())
            .find(|a| a.label == label)
    {
        a.skipped = Some(why.to_owned());
    }
}

/// The render pass hook of `probe`: records the pass of a captured frame.
pub(crate) fn before(
    sh: &Shared<'_>,
    _cb: &mut CommandBuffer,
    index: usize,
    pass: &Pass,
    draws: std::ops::Range<u32>,
    desc: &RenderPassDesc,
) -> Option<Box<dyn PassWatch>> {
    if !hooks_on() {
        return None;
    }
    pass::before(sh, index, pass, draws, desc)
}

/// Starts a frame. True when the frame is captured: the caller waits for the GPU at its end and calls [`finish`].
pub(crate) fn begin(frame: u64) -> bool {
    if FLAGS.load(Ordering::Relaxed) & (PENDING | RUNNING) == 0 {
        return false;
    }
    let mut st = state();
    // A frame that began and never finished (an error on the way) ends its capture here.
    if let Some(old) = st.run.take() {
        let _ = old.job.tx.send(Err(
            "the frame ended with an error before it was complete".to_owned()
        ));
    }
    set(RUNNING | HOOKS, false);
    let Some(job) = st.job.take() else {
        set(PENDING, false);
        return false;
    };
    set(PENDING, false);
    let hooks = !matches!(job.want, Want::Texture(_));
    st.run = Some(Run {
        job,
        frame,
        passes: Vec::new(),
        snaps: Vec::new(),
        bytes: 0,
    });
    set(RUNNING, true);
    set(HOOKS, hooks);
    true
}

/// What the encoder knows at the end of a captured frame, after the GPU is done with it.
pub(crate) struct Finish<'a> {
    pub(crate) plan: &'a PassPlan,
    pub(crate) frame: &'a Frame,
    pub(crate) stats: &'a EncodeStats,
    /// The GPU time of the frame's command buffers, when the wait gave it.
    pub(crate) gpu_ns: Option<u64>,
    /// The Metal texture of a texture of the game, as the frame left it.
    pub(crate) texture: &'a dyn Fn(TexId) -> Option<Texture>,
}

fn read(ctx: &Context, tex: &Texture) -> Result<Vec<u8>, String> {
    tex.debug_read_image(ctx, 0, 0).ok_or_else(|| {
        "the texture cannot be read back (compressed, stencil, multisample or memoryless)"
            .to_owned()
    })
}

fn image(ctx: &Context, key: Key, id: Option<u32>, tex: &Texture) -> Image {
    let d = tex.desc();
    Image {
        key,
        tex: id,
        format: d.format,
        width: d.width,
        height: d.height,
        data: read(ctx, tex),
    }
}

/// The presented texture of the frame: the texture of its last present command.
fn presented(frame: &Frame) -> Option<TexId> {
    frame.cmds.iter().rev().find_map(|c| match c {
        Cmd::Present(id) => Some(*id),
        _ => None,
    })
}

/// Ends the capture of the frame: reads the pictures back and sends them to the caller.
pub(crate) fn finish(ctx: &Context, fin: &Finish<'_>) {
    let Some(run) = state().run.take() else {
        set(RUNNING | HOOKS, false);
        return;
    };
    set(RUNNING | HOOKS, false);
    let Run {
        job,
        frame,
        mut passes,
        snaps,
        ..
    } = run;
    let info = FrameInfo {
        encode_ns: fin.stats.encode_ns,
        gpu_ns: fin.gpu_ns,
        draws: fin.stats.draws,
        render_passes: fin.stats.render_passes,
        compute_passes: fin.stats.compute_passes,
        command_buffers: fin.stats.command_buffers,
        pipeline_builds: fin.stats.pipeline_builds,
        textures_created: fin.stats.textures_created,
    };
    let mut images = Vec::new();
    if let Want::Texture(id) = job.want {
        let reply = match (fin.texture)(TexId(id)) {
            Some(t) => {
                images.push(image(ctx, Key::Texture(id), Some(id), &t));
                Ok(())
            }
            None => Err(format!("texture {id} is unknown: no frame used it")),
        };
        let _ = job.tx.send(reply.map(|()| Captured {
            want: job.want.clone(),
            frame,
            info,
            passes: Vec::new(),
            images,
        }));
        return;
    }
    let plan: Vec<(String, [u32; 2])> = fin
        .plan
        .passes
        .iter()
        .map(|p| (format!("{:?}", p.kind), [p.cmds.start, p.cmds.end]))
        .collect();
    passes = merge(&plan, passes);
    if job.want != Want::List {
        match presented(fin.frame) {
            Some(id) => match (fin.texture)(id) {
                Some(t) => images.push(image(ctx, Key::Final, Some(id.0), &t)),
                None => images.push(failed(Key::Final, id.0, "the presented texture is unknown")),
            },
            None => images.push(failed(Key::Final, 0, "the frame has no present command")),
        }
    }
    for s in snaps {
        images.push(Image {
            key: Key::Pass(s.pass, s.label),
            tex: Some(s.tex),
            format: s.format,
            width: s.width,
            height: s.height,
            data: read(ctx, &s.snapshot),
        });
    }
    let _ = job.tx.send(Ok(Captured {
        want: job.want.clone(),
        frame,
        info,
        passes,
        images,
    }));
}

fn failed(key: Key, id: u32, why: &str) -> Image {
    Image {
        key,
        tex: Some(id),
        format: Format::Rgba8Unorm,
        width: 0,
        height: 0,
        data: Err(why.to_owned()),
    }
}

/// Every pass of the plan, given as its kind and command range: the render passes that the hook recorded, and the
/// others by their kind.
fn merge(plan: &[(String, [u32; 2])], mut seen: Vec<PassRec>) -> Vec<PassRec> {
    plan.iter()
        .enumerate()
        .map(
            |(i, (kind, cmds))| match seen.iter().position(|r| r.index == i) {
                Some(at) => seen.swap_remove(at),
                None => PassRec {
                    index: i,
                    kind: kind.clone(),
                    cmds: *cmds,
                    ..PassRec::default()
                },
            },
        )
        .collect()
}

/// What [`request`] gives back: wait on it for the files.
pub struct Ticket {
    rx: Receiver<Reply>,
    want: Want,
    base: PathBuf,
}

/// Puts a request in the queue. It fails when another request waits or runs.
pub fn request(req: Request) -> Result<Ticket, String> {
    let base = match req.dir {
        Some(d) => std::path::absolute(&d).map_err(|e| format!("{}: {e}", d.display()))?,
        None => crate::diag::diag_dir().clone(),
    };
    let (tx, rx) = channel();
    let mut st = state();
    if st.job.is_some() || st.run.is_some() {
        return Err("another capture waits for a frame".to_owned());
    }
    st.job = Some(Job {
        want: req.want.clone(),
        tx,
    });
    set(PENDING, true);
    Ok(Ticket {
        rx,
        want: req.want,
        base,
    })
}

/// Takes a request back that no frame has taken yet. True when there was one.
fn cancel() -> bool {
    let mut st = state();
    set(PENDING, false);
    st.job.take().is_some()
}

impl Ticket {
    /// Waits for the frame, writes the files and gives the lines of the answer: the folder first. `List` writes
    /// nothing and gives the lines of the passes. It fails after `timeout` when no frame comes (the game is paused).
    pub fn wait(self, timeout: Duration) -> Result<Vec<String>, String> {
        let got = match self.rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) if cancel() => {
                return Err(format!(
                    "no frame was encoded in {} s (is the game paused or in a loading screen?)",
                    timeout.as_secs()
                ));
            }
            // A frame took the request and is not finished: it gets time for the read back.
            Err(RecvTimeoutError::Timeout) => self
                .rx
                .recv_timeout(Duration::from_secs(60))
                .map_err(|_| "the frame did not finish".to_owned())?,
            Err(RecvTimeoutError::Disconnected) => {
                return Err("the capture was dropped".to_owned());
            }
        };
        let captured = got?;
        if self.want == Want::List {
            let mut out = vec![format!(
                "frame {}: {} passes, {} draws",
                captured.frame,
                captured.passes.len(),
                captured.info.draws
            )];
            out.extend(captured.passes.iter().map(PassRec::line));
            return Ok(out);
        }
        let written = write::write(&self.base, &captured)?;
        let mut out = vec![written.dir.display().to_string()];
        out.extend(written.lines);
        Ok(out)
    }
}

/// The numbers of the last frames as text lines.
pub fn stats_lines() -> Vec<String> {
    stats::lines()
}

/// Makes the next frame a sampled frame: the per-draw categories of `NXKIT_DIAG` print for it.
pub fn sample_next() {
    crate::diag::force_sample();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_fills_the_passes_the_hook_did_not_see() {
        let plan = vec![
            ("Compute".to_owned(), [0, 5]),
            ("Render".to_owned(), [10, 15]),
            ("Copy".to_owned(), [20, 25]),
        ];
        let seen = vec![PassRec {
            index: 1,
            kind: "Render".into(),
            draws: 3,
            ..PassRec::default()
        }];
        let all = merge(&plan, seen);
        let kinds: Vec<&str> = all.iter().map(|p| p.kind.as_str()).collect();
        assert_eq!(kinds, ["Compute", "Render", "Copy"]);
        assert_eq!(all[1].draws, 3);
        assert_eq!(all[2].cmds, [20, 25]);
        assert_eq!(all[0].index, 0);
    }
}
