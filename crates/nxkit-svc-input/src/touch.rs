//! The touch screen and the gestures the SDK builds from it.
//!
//! The host pushes the touch points of its front end through [`crate::set_touches`]. A game reads them as
//! `TouchScreenState<N>` and as `GestureState` entries. Both are ported from nxnative (`hle_touch.c`). The panel is
//! 1280 x 720 and the points of the host are normalized to 0..1 from the top left of the picture.
//!
//! A game looks only at the newest state, so a finger that goes down and up between two reads would be lost. Every
//! finger is therefore reported once as Start and, after the lift, once more as End.
//!
//! The gesture rules are those of yuzu (`hid_core` `gesture_handler.cpp`), with one difference: a gesture stays put
//! until a finger moved [`SLOP`] pixels from where it went down, because a fingertip wobbles by a pixel or two.

/// One touch point of the host: a finger id below 16 and its position in 0..1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Touch {
    /// The finger id, 0 to 15. Points with a higher id are dropped.
    pub id: u32,
    /// Horizontal position, 0 (left) to 1 (right).
    pub x: f32,
    /// Vertical position, 0 (top) to 1 (bottom).
    pub y: f32,
}

const FINGERS: usize = 16;
const PANEL_W: i32 = 1280;
const PANEL_H: i32 = 720;
/// The size of one `TouchState` in bytes.
const TOUCH_STATE_SIZE: usize = 0x28;
/// The size of the header of a `TouchScreenState`.
const TOUCH_HEADER_SIZE: usize = 0x10;
/// The size of one `GestureState` in bytes.
pub(crate) const GESTURE_STATE_SIZE: usize = 0x60;
const SLOP: i32 = 8;
const HISTORY: usize = 16;

#[derive(Clone, Copy, Default, PartialEq)]
enum Phase {
    #[default]
    Empty,
    Down,
    Lifted,
}

#[derive(Clone, Copy, Default)]
struct Finger {
    phase: Phase,
    reported: bool,
    /// The same id went down again before its End was reported.
    again: bool,
    x: f32,
    y: f32,
    again_x: f32,
    again_y: f32,
}

/// The gesture recognizer's own view of a finger.
#[derive(Clone, Copy, Default)]
struct Watch {
    down: bool,
    /// Went down since the last sample. A press and lift between two samples still counts once.
    pending: bool,
    x: f32,
    y: f32,
}

mod kind {
    pub(super) const IDLE: i32 = 0;
    pub(super) const COMPLETE: i32 = 1;
    pub(super) const CANCEL: i32 = 2;
    pub(super) const TOUCH: i32 = 3;
    pub(super) const PRESS: i32 = 4;
    pub(super) const TAP: i32 = 5;
    pub(super) const PAN: i32 = 6;
    pub(super) const SWIPE: i32 = 7;
    pub(super) const PINCH: i32 = 8;
    pub(super) const ROTATE: i32 = 9;
}

mod direction {
    pub(super) const LEFT: i32 = 1;
    pub(super) const UP: i32 = 2;
    pub(super) const RIGHT: i32 = 3;
    pub(super) const DOWN: i32 = 4;
}

const NEW_TOUCH: u32 = 1 << 4;
const DOUBLE_TAP: u32 = 1 << 8;

/// One `GestureState`.
#[derive(Clone, Copy, Default)]
struct Gesture {
    sampling: i64,
    context: i64,
    kind: i32,
    direction: i32,
    x: i32,
    y: i32,
    dx: i32,
    dy: i32,
    vx: f32,
    vy: f32,
    attributes: u32,
    scale: f32,
    rotation: f32,
    points: i32,
    pt: [[i32; 2]; 4],
}

impl Gesture {
    fn to_bytes(self) -> [u8; GESTURE_STATE_SIZE] {
        let mut b = [0u8; GESTURE_STATE_SIZE];
        b[0..8].copy_from_slice(&self.sampling.to_le_bytes());
        b[8..16].copy_from_slice(&self.context.to_le_bytes());
        for (i, v) in [self.kind, self.direction, self.x, self.y, self.dx, self.dy].into_iter().enumerate() {
            b[16 + 4 * i..20 + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
        b[40..44].copy_from_slice(&self.vx.to_le_bytes());
        b[44..48].copy_from_slice(&self.vy.to_le_bytes());
        b[48..52].copy_from_slice(&self.attributes.to_le_bytes());
        b[52..56].copy_from_slice(&self.scale.to_le_bytes());
        b[56..60].copy_from_slice(&self.rotation.to_le_bytes());
        b[60..64].copy_from_slice(&self.points.to_le_bytes());
        for (i, p) in self.pt.iter().flatten().enumerate() {
            b[64 + 4 * i..68 + 4 * i].copy_from_slice(&p.to_le_bytes());
        }
        b
    }
}

/// The fingers of a sample.
#[derive(Clone, Copy, Default)]
struct Props {
    n: i32,
    pt: [[i32; 2]; 4],
    mx: i32,
    my: i32,
    dist: f32,
    angle: f32,
}

fn panel(v: f32, hi: i32) -> i32 {
    ((v * (hi + 1) as f32) as i32).clamp(0, hi)
}

/// The touch screen and gesture state.
pub(crate) struct Touches {
    fingers: [Finger; FINGERS],
    watch: [Watch; FINGERS],
    sampling: i64,
    last_read_ns: u64,
    history: [Gesture; HISTORY],
    written: u64,
    last: Props,
    went_down: Props,
    context: i64,
    force: bool,
    tap_ok: bool,
    slop_left: bool,
    updated_ns: u64,
    tapped_ns: u64,
    pan_dt: f32,
}

impl Default for Touches {
    fn default() -> Self {
        Touches {
            fingers: [Finger::default(); FINGERS],
            watch: [Watch::default(); FINGERS],
            sampling: 0,
            last_read_ns: 0,
            history: [Gesture::default(); HISTORY],
            written: 0,
            last: Props::default(),
            went_down: Props::default(),
            context: 0,
            force: false,
            tap_ok: false,
            slop_left: false,
            updated_ns: 0,
            tapped_ns: 0,
            pan_dt: 0.0,
        }
    }
}

impl Touches {
    /// Takes the whole set of touch points that are down now.
    pub(crate) fn apply(&mut self, points: &[Touch]) {
        let mut seen = [false; FINGERS];
        for t in points {
            let id = t.id as usize;
            if id >= FINGERS {
                continue;
            }
            seen[id] = true;
            let w = &mut self.watch[id];
            if !w.down {
                w.pending = true;
            }
            *w = Watch { down: true, pending: w.pending, x: t.x, y: t.y };
            let f = &mut self.fingers[id];
            if f.phase == Phase::Lifted && f.reported {
                f.again = true;
                f.again_x = t.x;
                f.again_y = t.y;
                continue;
            }
            if f.phase == Phase::Empty {
                f.phase = Phase::Down;
                f.reported = false;
            } else if f.phase == Phase::Lifted {
                f.phase = Phase::Down;
            }
            f.x = t.x;
            f.y = t.y;
        }
        for id in 0..FINGERS {
            if seen[id] {
                continue;
            }
            self.watch[id].down = false;
            let f = &mut self.fingers[id];
            if f.phase == Phase::Down {
                f.phase = Phase::Lifted;
            }
            f.again = false;
        }
    }

    /// The bytes of a `TouchScreenState<max>`: sampling number, point count and the points. Fingers beyond `max`
    /// wait for a later read.
    pub(crate) fn read(&mut self, max: usize, now_ns: u64) -> Vec<u8> {
        let max = max.clamp(1, FINGERS);
        let mut out = vec![0u8; TOUCH_HEADER_SIZE + TOUCH_STATE_SIZE * max];
        self.sampling += 1;
        let delta = if self.last_read_ns != 0 { now_ns.saturating_sub(self.last_read_ns) as i64 } else { 0 };
        self.last_read_ns = now_ns;
        let mut k = 0;
        for id in 0..FINGERS {
            if k == max {
                break;
            }
            let f = &mut self.fingers[id];
            if f.phase == Phase::Empty {
                continue;
            }
            let mut attributes = 0u32;
            if !f.reported {
                attributes = 1;
                f.reported = true;
            } else if f.phase == Phase::Lifted {
                attributes = 2;
            }
            let at = TOUCH_HEADER_SIZE + TOUCH_STATE_SIZE * k;
            k += 1;
            let s = &mut out[at..at + TOUCH_STATE_SIZE];
            s[0..8].copy_from_slice(&delta.to_le_bytes());
            s[8..12].copy_from_slice(&attributes.to_le_bytes());
            s[12..16].copy_from_slice(&(id as i32).to_le_bytes());
            s[16..20].copy_from_slice(&panel(f.x, PANEL_W - 1).to_le_bytes());
            s[20..24].copy_from_slice(&panel(f.y, PANEL_H - 1).to_le_bytes());
            s[24..28].copy_from_slice(&30i32.to_le_bytes());
            s[28..32].copy_from_slice(&30i32.to_le_bytes());
            if attributes & 2 != 0 {
                if f.again {
                    *f = Finger { phase: Phase::Down, x: f.again_x, y: f.again_y, ..Finger::default() };
                } else {
                    f.phase = Phase::Empty;
                }
            }
        }
        out[0..8].copy_from_slice(&self.sampling.to_le_bytes());
        out[8..12].copy_from_slice(&(k as i32).to_le_bytes());
        out
    }

    /// The fingers down now, or pressed since the last sample, in id order, at most four.
    fn sample_fingers(&mut self) -> Props {
        let mut g = Props::default();
        for w in &mut self.watch {
            if (w.down || w.pending) && g.n < 4 {
                g.pt[g.n as usize] = [panel(w.x, PANEL_W - 1), panel(w.y, PANEL_H - 1)];
                g.n += 1;
            }
            w.pending = false;
        }
        let n = g.n.max(1);
        for p in &g.pt[..g.n as usize] {
            g.mx += p[0] / n;
            g.my += p[1] / n;
        }
        for p in &g.pt[..g.n as usize] {
            g.dist += ((g.mx - p[0]) as f32).hypot((g.my - p[1]) as f32) / n as f32;
        }
        if g.n > 0 {
            g.angle = ((g.my - g.pt[0][1]) as f32).atan2((g.mx - g.pt[0][0]) as f32);
        }
        g
    }

    fn newest(&self) -> Option<Gesture> {
        (self.written > 0).then(|| self.history[((self.written - 1) % HISTORY as u64) as usize])
    }

    fn sample(&mut self, now: u64) {
        let mut g = self.sample_fingers();
        let le = self.newest();
        let ltype = le.map_or(kind::IDLE, |e| e.kind);
        let dt = if self.updated_ns != 0 { now.saturating_sub(self.updated_ns) as f32 / 1e9 } else { 0.0 };
        if g.n > 0 && g.n == self.last.n && !self.slop_left {
            let n = g.n as usize;
            let out = (0..n).any(|i| {
                (g.pt[i][0] - self.went_down.pt[i][0]).abs() > SLOP || (g.pt[i][1] - self.went_down.pt[i][1]).abs() > SLOP
            });
            if out {
                self.slop_left = true;
            } else {
                g = self.last;
            }
        }
        let mut upd = self.force || g.n != self.last.n || g.pt != self.last.pt;
        if !upd && ltype == kind::TOUCH && le.is_some_and(|e| e.points == 1) && dt > 0.5 {
            upd = self.tap_ok;
        }
        if !upd {
            return;
        }
        self.force = false;
        let le_or = le.unwrap_or_default();
        let mut s = Gesture { sampling: le.map_or(1, |e| e.sampling + 1), ..Gesture::default() };
        let mut ty = kind::IDLE;
        let mut attributes = 0u32;
        if g.n > 0 {
            if self.last.n == 0 {
                self.context += 1;
                ty = kind::TOUCH;
                self.went_down = g;
                self.slop_left = false;
                if ltype != kind::CANCEL {
                    attributes |= NEW_TOUCH;
                    self.tap_ok = true;
                }
            } else if g.n != self.last.n {
                ty = kind::CANCEL;
                self.tap_ok = false;
                g = Props::default();
            } else if g.pt != self.last.pt {
                ty = kind::PAN;
                s.dx = g.mx - le.map_or(g.mx, |e| e.x);
                s.dy = g.my - le.map_or(g.my, |e| e.y);
                if dt > 0.0 {
                    s.vx = s.dx as f32 / dt;
                    s.vy = s.dy as f32 / dt;
                }
                self.pan_dt = dt;
                if (g.dist - self.last.dist).abs() > 0.5 {
                    ty = kind::PINCH;
                    s.scale = if self.last.dist > 0.0 { g.dist / self.last.dist } else { 1.0 };
                }
                let turn = ((g.angle - self.last.angle) / (1.0 + g.angle * self.last.angle)).atan();
                if turn.abs() > 0.015 {
                    ty = kind::ROTATE;
                    s.scale = 0.0;
                    s.rotation = turn.to_degrees();
                }
            } else if ltype == kind::TOUCH {
                ty = kind::PRESS;
            }
        } else if self.last.n > 0 {
            match ltype {
                kind::TOUCH => {
                    self.force = true;
                    if !self.tap_ok {
                        ty = kind::CANCEL;
                    } else {
                        ty = kind::TAP;
                        g = self.last;
                        if self.tapped_ns != 0 && now.saturating_sub(self.tapped_ns) < 350_000_000 {
                            attributes |= DOUBLE_TAP;
                        }
                        self.tapped_ns = now;
                    }
                }
                kind::PRESS | kind::TAP | kind::SWIPE | kind::PINCH | kind::ROTATE => {
                    ty = kind::COMPLETE;
                    self.force = true;
                }
                kind::PAN => {
                    let t = self.pan_dt + dt;
                    let (vx, vy) = if t > 0.0 { (le_or.dx as f32 / t, le_or.dy as f32 / t) } else { (0.0, 0.0) };
                    self.force = true;
                    if vx.hypot(vy) <= 400.0 {
                        ty = kind::COMPLETE;
                    } else {
                        ty = kind::SWIPE;
                        g = self.last;
                        s.vx = vx;
                        s.vy = vy;
                        s.dx = le_or.dx;
                        s.dy = le_or.dy;
                        s.direction = if s.dx.abs() > s.dy.abs() {
                            if s.dx > 0 { direction::RIGHT } else { direction::LEFT }
                        } else if s.dy > 0 {
                            direction::DOWN
                        } else {
                            direction::UP
                        };
                    }
                }
                _ => {}
            }
        } else if ltype == kind::COMPLETE || ltype == kind::CANCEL {
            self.context += 1;
        }
        s.context = self.context;
        s.kind = ty;
        s.attributes = attributes;
        s.x = g.mx;
        s.y = g.my;
        s.points = g.n;
        s.pt = g.pt;
        self.last = g;
        self.updated_ns = now;
        self.history[(self.written % HISTORY as u64) as usize] = s;
        self.written += 1;
    }

    /// Up to `n` gesture states, newest first. The recognizer samples the fingers at each call.
    pub(crate) fn gestures(&mut self, n: usize, now_ns: u64) -> Vec<[u8; GESTURE_STATE_SIZE]> {
        if n == 0 {
            return Vec::new();
        }
        self.sample(now_ns);
        let k = (self.written.min(HISTORY as u64) as usize).min(n);
        (0..k as u64)
            .map(|i| self.history[((self.written - 1 - i) % HISTORY as u64) as usize].to_bytes())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, y: f32) -> Touch {
        Touch { id: 0, x, y }
    }

    fn attribute(read: &[u8], k: usize) -> u32 {
        let o = TOUCH_HEADER_SIZE + TOUCH_STATE_SIZE * k + 8;
        u32::from_le_bytes([read[o], read[o + 1], read[o + 2], read[o + 3]])
    }

    fn count(read: &[u8]) -> i32 {
        i32::from_le_bytes([read[8], read[9], read[10], read[11]])
    }

    #[test]
    fn a_press_and_lift_between_two_reads_is_start_then_end() {
        let mut t = Touches::default();
        t.apply(&[at(0.5, 0.5)]);
        t.apply(&[]);
        let a = t.read(16, 10);
        assert_eq!((count(&a), attribute(&a, 0)), (1, 1));
        let b = t.read(16, 20);
        assert_eq!((count(&b), attribute(&b, 0)), (1, 2));
        assert_eq!(count(&t.read(16, 30)), 0);
    }

    #[test]
    fn coordinates_map_to_the_panel() {
        let mut t = Touches::default();
        t.apply(&[at(0.5, 1.0)]);
        let r = t.read(1, 0);
        let x = i32::from_le_bytes([r[0x20], r[0x21], r[0x22], r[0x23]]);
        let y = i32::from_le_bytes([r[0x24], r[0x25], r[0x26], r[0x27]]);
        assert_eq!((x, y), (640, 719));
    }

    #[test]
    fn a_held_finger_reports_start_once() {
        let mut t = Touches::default();
        t.apply(&[at(0.1, 0.1)]);
        assert_eq!(attribute(&t.read(16, 1), 0), 1);
        assert_eq!(attribute(&t.read(16, 2), 0), 0);
    }

    #[test]
    fn a_tap_gives_touch_then_tap_then_complete() {
        let mut t = Touches::default();
        t.apply(&[at(0.5, 0.5)]);
        let a = t.gestures(16, 1_000_000);
        assert_eq!(a.len(), 1);
        assert_eq!(i32::from_le_bytes([a[0][16], a[0][17], a[0][18], a[0][19]]), kind::TOUCH);
        t.apply(&[]);
        let b = t.gestures(16, 2_000_000);
        assert_eq!(i32::from_le_bytes([b[0][16], b[0][17], b[0][18], b[0][19]]), kind::TAP);
        let c = t.gestures(16, 3_000_000);
        assert_eq!(i32::from_le_bytes([c[0][16], c[0][17], c[0][18], c[0][19]]), kind::COMPLETE);
    }

    #[test]
    fn gestures_are_newest_first_with_counting_numbers() {
        let mut t = Touches::default();
        t.apply(&[at(0.5, 0.5)]);
        t.gestures(1, 1);
        t.apply(&[]);
        let g = t.gestures(16, 2);
        assert!(g.len() >= 2);
        let s = |b: &[u8; GESTURE_STATE_SIZE]| i64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
        assert_eq!(s(&g[0]), s(&g[1]) + 1);
    }

    #[test]
    fn no_gestures_without_fingers() {
        let mut t = Touches::default();
        assert!(t.gestures(16, 1).is_empty());
    }
}
