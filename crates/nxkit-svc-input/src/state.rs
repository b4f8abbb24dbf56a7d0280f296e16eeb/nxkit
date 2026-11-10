//! The state of the input service: the platform, the slots, the numbers the game set and the counters of the states.
//!
//! One `Input` sits behind a mutex. A call from the game locks it, reads the controllers and answers. Nothing runs
//! between calls (R39).

use std::time::{Duration, Instant};

use crate::bind::{Action, Bindings, raw_from};
use crate::config::Config;
use crate::keys::{KEYBOARD_STATE_SIZE, KeySet};
use crate::motion::{MotionMap, MotionState, SixAxisData};
use crate::npad::{
    Layout, NpadState, PLAYERS, RawKeys, RawPad, STATE_SIZE, build_state, id_bit, merge, pick,
    to_stick,
};
use crate::pointer;
use crate::rumble::{Rumble, VibrationValue};
use crate::script;
use crate::seam;
use crate::shmem::SharedMemory;
use crate::slots::Slots;
use crate::sys::Platform;
use crate::touch::{GESTURE_STATE_SIZE, Touch, Touches};

/// The first sampling number. It stays far from 0 so that a game never takes the first states for stale ones.
const FIRST_SAMPLING: i64 = 1 << 16;
/// The controllers are looked up again when this much time has passed since the last lookup.
const REFRESH_EVERY: Duration = Duration::from_millis(100);
/// The interval of one six-axis sample, in nanoseconds.
pub(crate) const SIX_AXIS_SAMPLE_NS: u64 = 5_000_000;
/// The size of one `SixAxisSensorState` in bytes.
pub(crate) const SIX_AXIS_STATE_SIZE: usize = 0x60;
/// The size of one `MouseState` in bytes.
pub(crate) const MOUSE_STATE_SIZE: usize = 0x28;
/// The result of the controller applet when the player closes it: module 202, description 3101.
const CONTROLLER_CANCELED: u32 = 202 | 3101 << 9;
/// A motion sample older than this starts the orientation again.
const MOTION_STALE: f32 = 0.25;
/// The gyro sum, in revolutions per second, from which a controller is not at rest.
const REST_LIMIT: f32 = 0.01;

/// The motion state of one player.
#[derive(Default)]
struct MotionTrack {
    state: MotionState,
    last: Option<Instant>,
}

/// The bytes of a `SixAxisSensorState`.
pub(crate) fn six_axis_bytes(d: &SixAxisData, sampling: i64) -> [u8; SIX_AXIS_STATE_SIZE] {
    let mut b = [0u8; SIX_AXIS_STATE_SIZE];
    b[0..8].copy_from_slice(&(SIX_AXIS_SAMPLE_NS as i64).to_le_bytes());
    b[8..16].copy_from_slice(&sampling.to_le_bytes());
    let floats = d.acc.iter().chain(&d.gyro).chain(&d.angle).chain(&d.dir);
    for (i, f) in floats.enumerate() {
        b[16 + 4 * i..20 + 4 * i].copy_from_slice(&f.to_le_bytes());
    }
    b[88..92].copy_from_slice(&1u32.to_le_bytes());
    b
}

/// Everything the service knows.
pub(crate) struct Input {
    pub(crate) config: Config,
    bindings: Bindings,
    map: MotionMap,
    platform: Platform,
    slots: Slots,
    epoch: Instant,
    last_refresh: Option<Instant>,
    sampling: i64,
    keyboard_sampling: u64,
    mouse_sampling: i64,
    motion: [MotionTrack; PLAYERS],
    rumble: Rumble,
    /// The mask of `SetSupportedNpadIdType`.
    pub(crate) ids: u32,
    /// The mask of `SetSupportedNpadStyleSet`.
    pub(crate) style_set: u32,
    /// `NpadJoyHoldType`, `NpadCommunicationMode` and the handheld activation mode, as the game set them.
    pub(crate) hold_type: u32,
    pub(crate) communication_mode: u32,
    pub(crate) handheld_activation: u32,
    /// `PermitVibration` of the hid service.
    pub(crate) vibration_permitted: bool,
    /// The shared memory of the hid service, once a game asked for it.
    pub(crate) shm: Option<SharedMemory>,
    /// The writable and readable handle of the style set update event, once a game asked for it.
    pub(crate) style_event: Option<(u32, u32)>,
    /// The guest `SystemEventType` bound by `BindNpadStyleSetUpdateEvent`, per player.
    pub(crate) style_events: [Option<u64>; PLAYERS],
    touch: Touches,
    announced: u8,
}

impl Input {
    pub(crate) fn new(config: &Config) -> Input {
        Input {
            config: config.clone(),
            bindings: config.bindings(),
            map: config.motion_map(),
            platform: Platform::new(config),
            slots: Slots::default(),
            epoch: Instant::now(),
            last_refresh: None,
            sampling: FIRST_SAMPLING,
            keyboard_sampling: FIRST_SAMPLING as u64,
            mouse_sampling: FIRST_SAMPLING,
            motion: std::array::from_fn(|_| MotionTrack::default()),
            rumble: Rumble::default(),
            ids: 0x3ff,
            style_set: 0xffff_ffff,
            hold_type: 0,
            communication_mode: 0,
            handheld_activation: 0,
            vibration_permitted: true,
            shm: None,
            style_event: None,
            style_events: [None; PLAYERS],
            touch: Touches::default(),
            announced: 0,
        }
    }

    /// Nanoseconds since the service started. The six-axis sampling numbers count from it.
    pub(crate) fn now_ns(&self) -> u64 {
        self.epoch.elapsed().as_nanos() as u64
    }

    /// Looks up the controllers when the last lookup is older than 100 ms. The first call always looks.
    pub(crate) fn refresh(&mut self) {
        let due = self
            .last_refresh
            .is_none_or(|t| t.elapsed() >= REFRESH_EVERY);
        if !due {
            return;
        }
        self.last_refresh = Some(Instant::now());
        self.platform.refresh(&mut self.slots);
        let mask = self.slots.mask();
        crate::set_controller_connected(mask != 0);
        if mask != self.announced {
            let changed = mask ^ self.announced;
            self.announced = mask;
            for (player, event) in self.style_events.iter().enumerate() {
                if let (true, Some(event)) = (changed >> player & 1 == 1, event) {
                    seam::signal(*event);
                }
            }
            if let (Some((writable, _)), Some(kernel)) =
                (self.style_event, nxkit_abi::kernel::kernel())
            {
                let _ = kernel.signal_event(writable);
            }
        }
    }

    /// Which npad ids exist now.
    pub(crate) fn layout(&mut self) -> Layout {
        self.refresh();
        Layout {
            handheld: self.config.handheld,
            ids: self.ids,
            connected: self.slots.mask(),
        }
    }

    /// Replaces the supported ids by the ones of a `SetSupportedNpadIdType` call. A change puts every motor to idle.
    pub(crate) fn set_ids(&mut self, ids: &[u32]) {
        let mask = ids.iter().fold(0, |m, &id| m | id_bit(id));
        if mask != self.ids {
            self.ids = mask;
            self.stop_rumble();
        }
    }

    /// The next number of the sampling counter that the npads, the debug pad and the touch screen share.
    pub(crate) fn next_sampling(&mut self) -> i64 {
        self.sampling += 1;
        self.sampling
    }

    /// The reading of a player's controllers. A game with one player gets every controller as player 1.
    fn raw_pad(&self, player: usize, one_player: bool) -> RawPad {
        let own = self.platform.read_pad(player).unwrap_or_default();
        if player != 0 || !one_player {
            return own;
        }
        let mut pads = vec![own];
        pads.extend((1..PLAYERS).filter_map(|slot| self.platform.read_pad(slot)));
        merge(&pads)
    }

    /// The state of the npad `id` read as `style`. Without a controller of that style it is all zero, attributes
    /// included.
    pub(crate) fn npad_state(&mut self, id: u32, style: u32) -> NpadState {
        let layout = self.layout();
        let player = layout.player_of(id);
        if Layout::style_for(id, player) & style == 0 {
            return NpadState::default();
        }
        let Some(player) = player else {
            return NpadState::default();
        };
        let pad = self.raw_pad(player, layout.one_player());
        let keys = if player == 0 {
            self.player_one_keys()
        } else {
            None
        };
        build_state(style, &pad, keys.as_ref())
    }

    /// What the keyboard, the mouse and the scripted input (`NXKIT_INPUT_FILE`) give player 1. `None` when the
    /// keyboard is off and no file is set.
    fn player_one_keys(&self) -> Option<RawKeys> {
        let scripted = script::active();
        if !self.config.keyboard && !scripted {
            return None;
        }
        let held = if scripted {
            script::held(Instant::now())
        } else {
            Vec::new()
        };
        Some(if self.config.keyboard {
            self.keyboard_mouse(held)
        } else {
            raw_from(&held)
        })
    }

    /// What the keyboard and the mouse give player 1 in the layout of the title: the bound buttons and sticks, and
    /// the mouse camera on the right stick. `scripted` are the held actions of the input file; they join the bound
    /// ones before the sticks are worked out, so opposite directions cancel. The keyboard and the mouse give nothing
    /// while the window has no focus, the scripted actions stay.
    fn keyboard_mouse(&self, mut scripted: Vec<Action>) -> RawKeys {
        if !crate::focused() {
            return raw_from(&scripted);
        }
        let now = Instant::now();
        let (mouse, look) = pointer::with(|p| (p.buttons(now), p.look(now)));
        scripted.extend(self.bindings.held(&self.keys(), mouse));
        let mut raw = raw_from(&scripted);
        raw.sticks[2] = pick(raw.sticks[2], to_stick(look[0]));
        raw.sticks[3] = pick(raw.sticks[3], to_stick(look[1]));
        raw
    }

    /// The bytes of `n` states of the npad `id`, newest first. Each read advances the sampling number. The
    /// controller is read once and the entries repeat it.
    pub(crate) fn npad_states(&mut self, id: u32, style: u32, n: usize) -> Vec<[u8; STATE_SIZE]> {
        let state = self.npad_state(id, style);
        let newest = self.next_sampling();
        (0..n).map(|i| state.to_bytes(newest - i as i64)).collect()
    }

    /// The six-axis data of a player at this moment.
    pub(crate) fn six_axis(&mut self, player: usize) -> SixAxisData {
        if !self.config.motion {
            return SixAxisData::rest();
        }
        let Some(track) = self.motion.get_mut(player) else {
            return SixAxisData::rest();
        };
        let Some((acc, rot)) = self.platform.read_motion(player) else {
            *track = MotionTrack::default();
            return SixAxisData::rest();
        };
        let now = Instant::now();
        let mut dt = track
            .last
            .map_or(0.0, |t| now.duration_since(t).as_secs_f32());
        if dt > MOTION_STALE {
            track.state = MotionState::default();
            dt = 0.0;
        }
        track
            .state
            .update(&self.map, acc, rot, dt, self.config.motion_gain);
        track.last = Some(now);
        track.state.data()
    }

    /// The bytes of `n` six-axis states of a player, newest first. All are the same reading. The sampling number
    /// counts 5 ms steps of the service clock.
    pub(crate) fn six_axis_states(
        &mut self,
        player: usize,
        n: usize,
    ) -> Vec<[u8; SIX_AXIS_STATE_SIZE]> {
        let data = self.six_axis(player);
        let newest = (self.now_ns() / SIX_AXIS_SAMPLE_NS) as i64;
        (0..n)
            .map(|i| six_axis_bytes(&data, newest - i as i64))
            .collect()
    }

    /// True when the player's controller does not turn.
    pub(crate) fn at_rest(&mut self, player: usize) -> bool {
        self.six_axis(player)
            .gyro
            .iter()
            .map(|g| g.abs())
            .sum::<f32>()
            < REST_LIMIT
    }

    /// Stores the value of a motor and plays it. An unchanged value does nothing.
    pub(crate) fn vibrate(&mut self, player: usize, motor: usize, value: VibrationValue) {
        if self.rumble.set(player, motor, value) {
            self.play(player);
        }
    }

    /// Every motor of every controller to idle.
    pub(crate) fn stop_rumble(&mut self) {
        self.rumble.clear();
        for player in 0..PLAYERS {
            self.play(player);
        }
    }

    fn play(&mut self, player: usize) {
        if !self.config.rumble {
            return;
        }
        let levels = self
            .rumble
            .levels(player, self.config.gain(), self.config.min());
        self.platform.rumble(player, &levels);
    }

    /// True when a controller can rumble: the setting is on.
    pub(crate) fn rumble_available(&self) -> bool {
        self.config.rumble
    }

    /// The bytes of a `KeyboardState`. All zero when there is no keyboard.
    pub(crate) fn keyboard_state(&mut self) -> [u8; KEYBOARD_STATE_SIZE] {
        let keys = self.keys();
        self.keyboard_sampling += 1;
        keys.to_state_bytes(self.keyboard_sampling)
    }

    /// The keys that are down.
    pub(crate) fn keys(&self) -> KeySet {
        if !crate::focused() {
            return KeySet::default();
        }
        self.platform.read_keys().unwrap_or_default()
    }

    /// The bytes of a `MouseState`. Only the buttons are filled. Position, delta and wheel are 0.
    pub(crate) fn mouse_state(&mut self) -> [u8; MOUSE_STATE_SIZE] {
        let buttons = if crate::focused() {
            self.platform.read_mouse()
        } else {
            None
        };
        self.mouse_sampling += 1;
        let mut b = [0u8; MOUSE_STATE_SIZE];
        b[0..8].copy_from_slice(&self.mouse_sampling.to_le_bytes());
        b[32..36].copy_from_slice(&buttons.unwrap_or(0).to_le_bytes());
        let attributes: u32 = if buttons.is_some() { 0b11 } else { 0 };
        b[36..40].copy_from_slice(&attributes.to_le_bytes());
        b
    }

    /// The six-axis states behind a `SixAxisSensorHandle`: byte 0 is the bit number of the style, byte 1 the npad id.
    /// Empty when the handle is not connected.
    pub(crate) fn six_axis_handle_states(
        &mut self,
        handle: u32,
        n: usize,
    ) -> Vec<[u8; SIX_AXIS_STATE_SIZE]> {
        let layout = self.layout();
        let (bit, id) = (handle & 0xff, handle >> 8 & 0xff);
        if bit >= 32 || layout.style_of(id) >> bit & 1 == 0 {
            return Vec::new();
        }
        self.six_axis_states(layout.player_of(id).unwrap_or(PLAYERS), n)
    }

    /// `IsSixAxisSensorAtRest` of a handle.
    pub(crate) fn handle_at_rest(&mut self, handle: u32) -> bool {
        let layout = self.layout();
        self.at_rest(layout.player_of(handle >> 8 & 0xff).unwrap_or(PLAYERS))
    }

    /// The player and the motor behind a `VibrationDeviceHandle`: byte 0 is the style (FullKey 3, Handheld 4), byte 1
    /// the npad id and byte 2 the motor. `None` when no controller of that style is there.
    pub(crate) fn vibration_target(&mut self, handle: u32) -> Option<(usize, usize)> {
        let (kind, id, motor) = (handle & 0xff, handle >> 8 & 0xff, handle >> 16 & 0xff);
        if handle >> 24 != 0 || motor > 1 || !(3..=4).contains(&kind) {
            return None;
        }
        let layout = self.layout();
        let player = layout.player_of(id)?;
        (Layout::style_for(id, Some(player)) == 1 << (kind - 3)).then_some((player, motor as usize))
    }

    /// The handles of the vibration devices of an npad: two at most, none when rumble is off or the style is not
    /// the one of the controller.
    pub(crate) fn vibration_handles(&mut self, id: u32, style: u32, n: usize) -> Vec<u32> {
        let layout = self.layout();
        if n == 0 || style & layout.style_of(id) == 0 || !self.config.rumble {
            return Vec::new();
        }
        let base = id << 8 | if id == crate::npad::ID_HANDHELD { 4 } else { 3 };
        (0..n.min(2) as u32).map(|i| base | i << 16).collect()
    }

    /// Sends a value to the motor of a handle. In a game with one player, the controllers of the other slots follow
    /// player 1.
    pub(crate) fn send_vibration(&mut self, handle: u32, value: VibrationValue) {
        let Some((player, motor)) = self.vibration_target(handle) else {
            return;
        };
        let layout = self.layout();
        if player == 0 && layout.one_player() {
            for slot in 1..PLAYERS {
                if self.slots.owner_of(slot).is_some() {
                    self.vibrate(slot, motor, value);
                }
            }
        }
        self.vibrate(player, motor, value);
    }

    /// The last value of the motor of a handle, idle when it has no controller.
    pub(crate) fn actual_vibration(&mut self, handle: u32) -> VibrationValue {
        match self.vibration_target(handle) {
            Some((player, motor)) => self.rumble.get(player, motor),
            None => VibrationValue::IDLE,
        }
    }

    /// Takes the touch points of the host.
    pub(crate) fn set_touches(&mut self, points: &[Touch]) {
        self.touch.apply(points);
    }

    /// The bytes of a `TouchScreenState<n>`. The touch screen is out of reach when docked, so the state has only
    /// its header then (sampling number and a count of 0).
    pub(crate) fn touch_state(&mut self, n: usize) -> Vec<u8> {
        if self.config.handheld {
            return self.touch.read(n, self.now_ns());
        }
        let mut header = vec![0u8; 0x10];
        header[0..8].copy_from_slice(&self.next_sampling().to_le_bytes());
        header
    }

    /// The gesture states, newest first. Gestures come from the touch screen, so only in handheld mode.
    pub(crate) fn gestures(&mut self, n: usize) -> Vec<[u8; GESTURE_STATE_SIZE]> {
        if self.config.handheld {
            self.touch.gestures(n, self.now_ns())
        } else {
            Vec::new()
        }
    }

    /// The answer of the controller applet: the result code, the player count and the id of player 1. The applet
    /// is answered as a player who closes it when fewer players are connected than the game wants at least.
    pub(crate) fn controller_support(&mut self, min: i8, max: i8) -> (u32, i32, u32) {
        let layout = self.layout();
        let mut count = layout.reported_count() as i32;
        let canceled = i32::from(min) > count;
        if max > 0 && count > i32::from(max) {
            count = i32::from(max);
        }
        (
            if canceled { CONTROLLER_CANCELED } else { 0 },
            count,
            layout.connected_id(),
        )
    }

    /// Renews the rumble pulses. The host calls this on each display tick.
    pub(crate) fn tick(&mut self) {
        self.refresh();
        for player in 0..PLAYERS {
            if self.slots.owner_of(player).is_some() {
                self.play(player);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> Input {
        Input::new(&Config::default())
    }

    #[test]
    fn no_controller_gives_player_one_a_state() {
        let mut i = input();
        let s = i.npad_state(0, crate::npad::style::FULL_KEY);
        assert_eq!(
            s.attributes,
            crate::npad::attributes_for(crate::npad::style::FULL_KEY)
        );
        assert_eq!(s.buttons, 0);
    }

    #[test]
    fn a_style_mismatch_is_all_zero() {
        let mut i = input();
        assert_eq!(
            i.npad_state(0, crate::npad::style::HANDHELD),
            NpadState::default()
        );
        assert_eq!(
            i.npad_state(3, crate::npad::style::FULL_KEY),
            NpadState::default()
        );
    }

    #[test]
    fn states_count_down_from_the_newest_sampling() {
        let mut i = input();
        let a = i.npad_states(0, crate::npad::style::FULL_KEY, 3);
        let b = i.npad_states(0, crate::npad::style::FULL_KEY, 1);
        let s = |x: &[u8; STATE_SIZE]| {
            i64::from_le_bytes([x[0], x[1], x[2], x[3], x[4], x[5], x[6], x[7]])
        };
        assert_eq!(s(&a[0]), s(&a[1]) + 1);
        assert_eq!(s(&a[1]), s(&a[2]) + 1);
        assert_eq!(s(&b[0]), s(&a[0]) + 1);
    }

    #[test]
    fn changing_the_ids_replaces_the_mask() {
        let mut i = input();
        i.set_ids(&[0, 0x20]);
        assert_eq!(i.ids, 1 | 1 << 8);
        i.set_ids(&[]);
        assert_eq!(i.ids, 0);
    }

    #[test]
    fn six_axis_without_motion_is_at_rest() {
        let mut i = input();
        assert!(i.at_rest(0));
        let b = i.six_axis_states(0, 2);
        assert_eq!(b.len(), 2);
        assert_eq!(&b[0][0..8], &(SIX_AXIS_SAMPLE_NS as i64).to_le_bytes());
        assert_eq!(&b[0][88..92], &1u32.to_le_bytes());
    }

    #[test]
    fn the_mouse_state_counts_its_samples() {
        let mut i = input();
        let m = i.mouse_state();
        assert_eq!(&m[0..8], &(FIRST_SAMPLING + 1).to_le_bytes());
        assert_eq!(&m[8..32], &[0u8; 24]);
    }
}
