//! An example patch module of the nxkit host (W37).
//!
//! It does two things:
//!
//! - It skips every draw whose fragment program has the key `SKIP_FRAGMENT`.
//! - It logs each render pass of the first frames after the load: the index, the attachments and the draw count.
//!
//! Change `SKIP_FRAGMENT`, run `cargo build --release` in this folder and `tools/nxkit-ctl patch reload`. The game
//! keeps running. `PATCH_EXAMPLE_SKIP_FS` in the environment of the host overrides the key at load.
//!
//! A hook is an `extern "C"` function. A panic in it aborts the whole game, so a patch keeps its hooks simple.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use nxkit_patch_abi::{DrawAnswer, DrawInfo, FrameBegin, HostApi, PassAnswer, PassInfo, PatchTable};

/// The key of the fragment program whose draws the patch skips. Empty skips nothing.
const SKIP_FRAGMENT: &str = "";

/// The passes of this many frames after the load are logged.
const LOG_FRAMES: u64 = 3;

/// The key that the patch skips: the environment of the host at load, else `SKIP_FRAGMENT`.
static SKIP: OnceLock<String> = OnceLock::new();

/// The log function of the host. A patch can keep its pointer: it stays valid while the patch is loaded.
static LOG: OnceLock<extern "C" fn(*const u8, usize)> = OnceLock::new();

/// The first frame that the patch saw, so that it logs only the passes of the first frames.
static FIRST_FRAME: AtomicU64 = AtomicU64::new(0);

/// The draws that the patch skipped since the load.
static SKIPPED: AtomicU64 = AtomicU64::new(0);

fn log(line: &str) {
    if let Some(f) = LOG.get() {
        f(line.as_ptr(), line.len());
    }
}

extern "C" fn frame_begin(info: &FrameBegin) {
    let _ = FIRST_FRAME.compare_exchange(0, info.frame, Ordering::Relaxed, Ordering::Relaxed);
}

extern "C" fn pass(info: &PassInfo, _answer: &mut PassAnswer) {
    if info.frame >= FIRST_FRAME.load(Ordering::Relaxed) + LOG_FRAMES {
        return;
    }
    let mut line = format!(
        "frame {} pass {} draws {} programs {}:",
        info.frame, info.index, info.draw_count, info.programs_total
    );
    for a in info.attachments() {
        line.push_str(&format!(
            " [slot {} tex {} {}x{} {}]",
            a.slot,
            a.tex_id,
            a.width,
            a.height,
            a.format.as_str()
        ));
    }
    log(&line);
}

extern "C" fn draw(info: &DrawInfo, answer: &mut DrawAnswer) {
    let key = SKIP.get().map_or("", String::as_str);
    if !key.is_empty() && info.programs.fragment.as_str() == key {
        answer.skip();
        SKIPPED.fetch_add(1, Ordering::Relaxed);
    }
}

nxkit_patch_abi::export_patch!(init);

fn init(host: &HostApi, table: &mut PatchTable) -> bool {
    let _ = LOG.set(host.log_raw);
    let key = std::env::var("PATCH_EXAMPLE_SKIP_FS").unwrap_or_else(|_| SKIP_FRAGMENT.to_string());
    log(&format!("patch-example loaded, skips fragment {key:?}"));
    let _ = SKIP.set(key);
    table.set_name("patch-example");
    table.frame_begin = Some(frame_begin);
    table.pass = Some(pass);
    table.draw = Some(draw);
    true
}
