//! The layer of the game as `nn::vi` sets it up and the NVN present reads it: the crop of the layer and the history of
//! the presented frames. The `nn::vi` take-overs (`crate::gfx::vi`) write and read it, and the present of NVN
//! (`crate::nvn::queue`) asks it for the crop and counts each frame, so it sits below both.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use nxkit_nvn::backend::Rect;

// Process-wide (W26): `nn::vi::SetLayerCrop` is a C function of the game with no context. The present reads it.
/// The layer crop as `x | y << 16 | w << 32 | h << 48`. 0 means none.
pub(crate) static CROP: AtomicU64 = AtomicU64::new(0);

pub(crate) fn layer_crop() -> [i32; 4] {
    let v = CROP.load(Ordering::Relaxed);
    [
        (v & 0xffff) as i32,
        (v >> 16 & 0xffff) as i32,
        (v >> 32 & 0xffff) as i32,
        (v >> 48) as i32,
    ]
}

/// The packed crop of `SetLayerCrop`. A negative value, an empty size or a value above 0xffff gives 0 (no crop).
pub(crate) fn pack_crop(x: i32, y: i32, w: i32, h: i32) -> u64 {
    if x < 0 || y < 0 || w <= 0 || h <= 0 || x > 0xffff || y > 0xffff || w > 0xffff || h > 0xffff {
        return 0;
    }
    x as u64 | (y as u64) << 16 | (w as u64) << 32 | (h as u64) << 48
}

// Once-guard of the note below.
static CROP_NOTE: AtomicBool = AtomicBool::new(false);

/// The crop that a present shows: the crop of the window, else the crop of the layer (`SetLayerCrop`). On the console
/// both become the crop of the buffer that is queued to the layer.
///
/// Link's Awakening crops only its layer. In handheld mode it draws 1280x720 into its 1920x1080 window textures and
/// crops the layer to that. A layer crop that does not fit the texture is reported once and the whole texture is shown.
/// `texture` is the size of the window texture, None when it is not known.
pub(crate) fn present_crop(window: Rect, texture: Option<(u32, u32)>) -> Rect {
    if window.width != 0 || window.height != 0 {
        return window;
    }
    let [x, y, w, h] = layer_crop();
    if w == 0 || h == 0 {
        return window;
    }
    let fits = texture.is_some_and(|(tw, th)| {
        i64::from(x) + i64::from(w) <= i64::from(tw) && i64::from(y) + i64::from(h) <= i64::from(th)
    });
    if !fits {
        if !CROP_NOTE.swap(true, Ordering::Relaxed) {
            let (tw, th) = texture.unwrap_or((0, 0));
            // the line of `crate::gfx::log`, once
            eprintln!(
                "[gfx] nxkit-rt: warning: vi layer crop {x},{y} {w}x{h} does not fit the window texture {tw}x{th}: the present shows the whole texture"
            );
        }
        return window;
    }
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

/// The frames that the history keeps.
pub(crate) const HISTORY: usize = 8;

#[derive(Clone, Copy, Default)]
struct Entry {
    frame: u64,
    queued_ns: u64,
    /// The guest time at which the frame was first seen after it was queued. 0 until then.
    seen_ns: u64,
}

/// The frames that the game presented and the last `HISTORY` of them.
pub(crate) struct Frames {
    pub(crate) count: u64,
    ring: [Entry; HISTORY],
}

impl Frames {
    pub(crate) const fn new() -> Self {
        Self {
            count: 0,
            ring: [Entry {
                frame: 0,
                queued_ns: 0,
                seen_ns: 0,
            }; HISTORY],
        }
    }

    /// Counts a present at the guest time `now`. The result is its frame number, which starts at 1.
    pub(crate) fn queue(&mut self, now: u64) -> u64 {
        self.count += 1;
        let frame = self.count;
        self.ring[frame as usize % HISTORY] = Entry {
            frame,
            queued_ns: now,
            seen_ns: 0,
        };
        frame
    }

    /// The `FrameInfo` records of the newest frames first, at most `n`. A frame counts as shown from the first call that
    /// sees it, because the backend does not report when a frame reaches the screen. `now` is the guest time.
    pub(crate) fn list(&mut self, n: usize, now: u64, ticks: impl Fn(u64) -> u64) -> Vec<[u64; 5]> {
        let latest = self.count;
        let mut out = Vec::new();
        let mut f = latest;
        while f > 0 && latest - f < HISTORY as u64 && out.len() < n {
            let e = &mut self.ring[f as usize % HISTORY];
            if e.frame == f {
                if e.seen_ns == 0 {
                    e.seen_ns = now.max(1);
                }
                let shown = ticks(e.seen_ns);
                out.push([2, f, ticks(e.queued_ns), shown, shown]);
            }
            f -= 1;
        }
        out
    }
}

// Process-wide (W26): `nn::vi::GetLatestFrameNumber` and `ListFrameHistory` are C functions of the game.
static FRAMES: Mutex<Frames> = Mutex::new(Frames::new());

pub(crate) fn frames() -> std::sync::MutexGuard<'static, Frames> {
    FRAMES.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The game presents a frame at the guest time `now`: it is the next frame of the layer. Called by
/// `nvnQueuePresentTexture`.
pub(crate) fn frame_queued(now: u64) {
    frames().queue(now);
}
