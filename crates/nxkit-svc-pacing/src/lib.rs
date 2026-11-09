//! The `pacing` service of nxkit: the vsync of the display and the frame step of the game (R30).
//!
//! The service has SDK-level functions only (R14), so [`services`] is empty. It does three things.
//!
//! - It gives the game the vsync event of `nn::vi::GetDisplayVsyncEvent` and signals it from the ticks of the display
//!   link, at the game's own vsync rate (see [`vsync`]). A game at 60 Hz on a display of 120 Hz gets an event on every
//!   second tick, so it steps its logic once per frame and runs at the speed of the Switch.
//! - It writes the step of each frame into the state page of a game that has frame step data (see [`state`] and
//!   [`mode`]). A game with such data runs at 120 fps and still steps right.
//! - It decides how long each frame stays on screen when the load of the frames asks for it (the port of the engine's
//!   `fstep_tick` in [`load`] and [`fstep`], R30a), and it writes the target frame rate into the frame limiter of an engine
//!   that has no frame step ([`limits`], the port of `hle_fpslock.c`).
//! - It counts what it did ([`snapshot`]), as part of the measurement of R16.
//!
//! `hle_fpslock.c` exports no SDK function. It only writes the guest's memory, so [`exports`] has the vsync
//! event alone.
//!
//! What the host does. It calls [`init`] before the game starts. It calls [`on_display_tick`] from the sink that it gave
//! to the `Pacer` of `nxkit-metal` on every tick of the display link. It calls [`frame_started`] when the game starts a
//! frame, and [`set_rates`] when the plan of `nxkit-metal` changes. When the title asks for a load-driven step or has an
//! engine limiter, it calls [`frame_presented`] once for each presented frame, and reads [`hold`], [`present_vsyncs`] and
//! [`deadline_vsyncs`] to pace the presents.
//!
//! The binding of the game's `SystemEventType` to a kernel event has to run SDK code of the game
//! (`nn::os::CreateSystemEvent`), because the game's structure is the SDK's own. `nxkit-abi` has no call into the
//! game's code. So the host gives [`Config::bind_system_event`], which makes the binding and returns the writable handle.
//! The service signals that handle through the kernel of `nxkit-abi`. A signal that fails ends the worker of the event,
//! because the game destroyed it. A handle number that the game reuses before the next tick would be signalled by
//! mistake. The engine closes that gap with a binding identity. This service does not and takes the gap as open.

use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread;

use nxkit_abi::export::Export;
use nxkit_abi::guest::Fault;
use nxkit_abi::ipc::Named;
use nxkit_abi::kernel::kernel;

pub mod fstep;
pub mod interval;
pub mod limits;
pub mod load;
pub mod mode;
pub mod notice;
pub mod refresh;
pub mod state;
pub mod vstep;
pub mod vsync;
pub mod watch;

pub use interval::PresentRule;
pub use limits::{LimitEvent, Limits, LimitsConfig};
pub use load::{LoadConfig, LoadStats, Outcome, PresentSample};
pub use mode::{BadTarget, MAX_VARIABLE_FPS, StepMode, Stepper};
pub use notice::Notice;
pub use state::StatePage;
pub use vstep::{STEP_MAX, STEP_MIN, VariableStep};
pub use vsync::ticks_per_vsync;

use vsync::{Rates, TickClock, Wait, wait_vsync};

/// The Switch mangled name of `nn::vi::GetDisplayVsyncEvent(nn::os::SystemEventType*, nn::vi::Display*)`.
pub const VSYNC_EVENT: &str =
    "_ZN2nn2vi20GetDisplayVsyncEventEPNS_2os15SystemEventTypeEPNS0_7DisplayE";

/// The result that a failing call gives. It is the one failure of the host (`RES_FAIL`).
const RES_FAIL: u32 = 0x2A2;

/// Binds the game's `SystemEventType` at the guest address to a kernel event and returns its writable handle. It runs
/// on a guest thread. It is the host's, because it executes SDK code of the game. The error is a Horizon result code.
pub type BindSystemEvent = fn(event: u64) -> Result<u32, u32>;

/// What the host gives the service.
#[derive(Clone, Debug)]
pub struct Config {
    /// Binds a game's system event to a kernel event.
    pub bind_system_event: BindSystemEvent,
    /// The guest address of the frame step state page, for a title that has frame step data.
    pub state_page: Option<u64>,
    /// The rate of the display link in Hz.
    pub display_hz: f32,
    /// The vsync rate of the game in Hz. A Switch game has 60.
    pub vsync_hz: f32,
    /// Who decides the step of a frame. It needs a state page unless it is [`StepMode::Own`] or [`StepMode::Load`].
    pub step: StepMode,
    /// The engine limiters of the main module, for a game that has no frame step. The host builds it from the loader's
    /// facts and the title's profile.
    pub limits: Option<LimitsConfig>,
    /// `present_interval` of the title is `display`: with no cap, one frame for each tick of the link.
    pub display_mode: bool,
}

/// A failure of the service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// [`init`] ran before.
    AlreadyInitialised,
    /// A call needs [`init`] first.
    NotInitialised,
    /// The step mode needs a state page and the config has none.
    NoStatePage,
    /// A rate is zero, negative or not a number.
    BadRate,
    /// A load-driven step at 120 fps counts ticks of 1/120 s and has no state page, and the config or the call gave one.
    PageAt120,
    /// The state page is outside guest memory.
    Guest(Fault),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyInitialised => f.write_str("the pacing service is already initialised"),
            Self::NotInitialised => f.write_str("the pacing service is not initialised"),
            Self::NoStatePage => f.write_str("the step mode needs a frame step state page"),
            Self::BadRate => f.write_str("a rate of the pacing service must be a positive number"),
            Self::PageAt120 => {
                f.write_str("a load-driven step at 120 fps has no frame step state page")
            }
            Self::Guest(e) => write!(f, "the frame step state page: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<Fault> for Error {
    fn from(e: Fault) -> Self {
        Self::Guest(e)
    }
}

/// What the service counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// Ticks of the display link.
    pub ticks: u64,
    /// Vsync events that the service signalled.
    pub signalled: u64,
    /// Vsync events that came from the timer, because no tick came for four vsync periods.
    pub self_clocked: u64,
    /// Vsync events that are bound and running.
    pub events: u32,
    /// Ticks of the display link that make one vsync of the game.
    pub ticks_per_vsync: u32,
    /// The vsyncs that the last presented frame stays on screen, 0 before the first present or without a load-driven step.
    pub hold: i32,
    /// The cap in fps that the presents follow, 0 for none.
    pub cap: u32,
}

struct Shared {
    bind: BindSystemEvent,
    clock: TickClock,
    rates: Rates,
    running: Mutex<Vec<u32>>,
    signalled: AtomicU64,
    self_clocked: AtomicU64,
    stepper: Mutex<Option<Stepper>>,
    limits: Mutex<Option<Limits>>,
    hold: AtomicI32,
    cap: AtomicU32,
    load: bool,
    step_hz: u32,
    display_mode: bool,
    notices: Vec<Notice>,
}

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

#[cfg(test)]
pub(crate) static GUEST_LOCK: Mutex<()> = Mutex::new(());

fn rate_ok(hz: f32) -> bool {
    hz.is_finite() && hz > 0.0
}

/// Starts the service. The host calls it once before the game starts.
pub fn init(config: &Config) -> Result<(), Error> {
    if !rate_ok(config.display_hz) || !rate_ok(config.vsync_hz) {
        return Err(Error::BadRate);
    }
    let stepper = match (config.state_page, config.step) {
        (Some(base), mode) => Some(Stepper::new(StatePage::new(base)?, mode)?),
        (None, StepMode::Own) => None,
        (None, StepMode::Load(c)) => Some(Stepper::without_page(c)),
        (None, _) => return Err(Error::NoStatePage),
    };
    let mut notices = Vec::new();
    let limits = config.limits.as_ref().map(|c| {
        let (l, mut n) = Limits::new(c);
        notices.append(&mut n);
        l
    });
    let (load, step_hz, mut cap) = match config.step {
        StepMode::Load(c) => (true, if c.at_120() { 120 } else { 60 }, c.target),
        _ => (false, 60, 0),
    };
    if !load && let Some(l) = &limits {
        let (c, notice) = interval::engine_present_cap(l.target(), l.has_engine());
        notices.extend(notice);
        cap = c;
    }
    let shared = Arc::new(Shared {
        bind: config.bind_system_event,
        clock: TickClock::default(),
        rates: Rates::new(config.display_hz, config.vsync_hz),
        running: Mutex::new(Vec::new()),
        signalled: AtomicU64::new(0),
        self_clocked: AtomicU64::new(0),
        stepper: Mutex::new(stepper),
        limits: Mutex::new(limits),
        hold: AtomicI32::new(0),
        cap: AtomicU32::new(cap),
        load,
        step_hz,
        display_mode: config.display_mode,
        notices,
    });
    SHARED.set(shared).map_err(|_| Error::AlreadyInitialised)
}

fn shared() -> Result<&'static Arc<Shared>, Error> {
    SHARED.get().ok_or(Error::NotInitialised)
}

/// One tick of the display link. The host calls it from the sink of the `Pacer`, on every tick.
pub fn on_display_tick() -> Result<(), Error> {
    shared()?.clock.tick();
    Ok(())
}

/// The refresh rate of the screen that shows the game, 120 until the host sets it.
static SCREEN_HZ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(120);

/// Sets the refresh rate of the screen that shows the game. A rate that is not a multiple of the step rate (144 Hz)
/// shows a frame on time one refresh longer now and then, which the frame step must not take for lateness.
pub fn set_screen_hz(hz: u32) {
    if hz > 0 {
        SCREEN_HZ.store(hz, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The refresh rate of the screen that shows the game.
pub fn screen_hz() -> u32 {
    SCREEN_HZ.load(std::sync::atomic::Ordering::Relaxed)
}

/// Sets the display rate and the vsync rate of the game. The host calls it when the plan changes.
pub fn set_rates(display_hz: f32, vsync_hz: f32) -> Result<(), Error> {
    if !rate_ok(display_hz) || !rate_ok(vsync_hz) {
        return Err(Error::BadRate);
    }
    shared()?.rates.set(display_hz, vsync_hz);
    Ok(())
}

/// Changes who decides the step of a frame. It needs a state page, except for the game's own step
/// and a load-driven step.
pub fn set_step_mode(mode: StepMode) -> Result<(), Error> {
    let mut g = shared()?
        .stepper
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    match g.as_mut() {
        Some(s) => s.set_mode(mode),
        None if mode == StepMode::Own => Ok(()),
        None => Err(Error::NoStatePage),
    }
}

/// The game starts a frame at `start_ns`, a monotonic time in nanoseconds. Writes the step of the frame. Does nothing
/// when the service has no state page and the mode is the game's own.
pub fn frame_started(start_ns: u64) -> Result<(), Error> {
    let mut g = shared()?
        .stepper
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    match g.as_mut() {
        Some(s) => Ok(s.frame_started(start_ns)?),
        None => Ok(()),
    }
}

/// Whether the variable step takes its time from the shown frames. Read without a lock by [`frame_shown`].
static BY_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Makes the variable step (`StepMode::Variable`) measure its time at the presentation: the step of a frame is the time
/// between the last two frames the display showed ([`frame_shown`]), not the time between two frame starts. The host
/// turns it on when frames are shown as soon as they are ready (variable refresh). The variable step starts again on a
/// change. Does nothing without a state page.
pub fn set_step_by_shown(on: bool) {
    BY_SHOWN.store(on, Ordering::Relaxed);
    if let Ok(s) = shared()
        && let Some(st) = s
            .stepper
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
    {
        st.set_by_shown(on);
    }
}

/// The display showed a frame at `shown_ns`, a monotonic time in nanoseconds on the clock of [`frame_started`]. A cheap
/// no-op while [`set_step_by_shown`] is off.
pub fn frame_shown(shown_ns: u64) {
    if !BY_SHOWN.load(Ordering::Relaxed) {
        return;
    }
    if let Ok(s) = shared()
        && let Some(st) = s
            .stepper
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
    {
        st.frame_shown(shown_ns);
    }
}

/// The game starts a frame at `start_ns`. The first acquire of the frame waited `wait_ns` for the pacing, and the
/// load-driven step takes that wait off the cost of the frame.
pub fn frame_started_waited(start_ns: u64, wait_ns: u64) -> Result<(), Error> {
    let mut g = shared()?
        .stepper
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    match g.as_mut() {
        Some(s) => Ok(s.frame_started_waited(start_ns, wait_ns)?),
        None => Ok(()),
    }
}

/// What a present decided.
#[derive(Clone, Debug, PartialEq)]
pub struct Presented {
    /// The load-driven step's decision. `None` in the other modes.
    pub outcome: Option<Outcome>,
    /// What the host wrote into the engine limiters, for the log.
    pub limits: Vec<LimitEvent>,
}

/// A frame is presented. The host calls it once for each present, before it takes the period of the frame. It runs the
/// load-driven step (which writes the step into the state page) and then writes the target into the engine limiters.
pub fn frame_presented(sample: &PresentSample) -> Result<Presented, Error> {
    let s = shared()?;
    let outcome = match s
        .stepper
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
    {
        Some(st) => st.present(sample)?,
        None => None,
    };
    if let Some(o) = &outcome {
        s.hold.store(o.hold, Ordering::Relaxed);
        s.cap.store(o.cap, Ordering::Relaxed);
    }
    let limits = match s
        .limits
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
    {
        Some(l) => l.tick()?,
        None => Vec::new(),
    };
    Ok(Presented { outcome, limits })
}

/// The vsyncs that the last presented frame stays on screen, 0 before the first present or without a load-driven step.
/// The unit is 1/60 s, or 1/120 s at 120 fps. The `Pacer` of `nxkit-metal` shows a frame for `hold` times the ticks of the
/// link that make one vsync of that unit.
pub fn hold() -> Result<i32, Error> {
    Ok(shared()?.hold.load(Ordering::Relaxed))
}

/// The frame rate that the engine limiters take now at a target of 120: 120 while the hold is one tick, else 60.
pub fn fps_high_now() -> Result<u32, Error> {
    Ok(interval::high_target_now(
        u32::try_from(hold()?).unwrap_or(0),
    ))
}

/// Whether the host writes the target into an engine limiter of the game. Without one and without a state page nothing
/// makes the game follow a cap.
pub fn engine_limiter() -> Result<bool, Error> {
    Ok(shared()?
        .limits
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .is_some_and(Limits::has_engine))
}

/// The notices that [`init`] made, for the log. They carry the refused targets and the limiters that were not found.
pub fn notices() -> Result<Vec<Notice>, Error> {
    Ok(shared()?.notices.clone())
}

fn rule(s: &Shared) -> PresentRule {
    PresentRule {
        cap: s.cap.load(Ordering::Relaxed),
        hold: u32::try_from(s.hold.load(Ordering::Relaxed)).unwrap_or(0),
        step_on: s.load,
        step_hz: s.step_hz,
        display_mode: s.display_mode,
    }
}

/// The vsyncs that a frame stays on screen (`present_itv`). `game_interval` is the present interval that the
/// game set, 0 when it set none. The unit is the one of [`hold`].
pub fn present_vsyncs(game_interval: u32) -> Result<u32, Error> {
    Ok(rule(shared()?).vsyncs(game_interval))
}

/// The vsyncs that the thread of the acquire declares to the scheduler as its deadline. A hold that late frames raised does
/// not move the deadline, so the performance controller keeps the game on the fast cores.
pub fn deadline_vsyncs(game_interval: u32) -> Result<u32, Error> {
    let s = shared()?;
    let fixes = s
        .stepper
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .map_or(Ok(false), Stepper::game_fixes_step)?;
    Ok(rule(s).deadline_vsyncs(game_interval, fixes))
}

/// What the load-driven step counted so far. `None` in the other modes.
pub fn load_stats() -> Result<Option<LoadStats>, Error> {
    Ok(shared()?
        .stepper
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .and_then(Stepper::load)
        .map(load::LoadStepper::stats))
}

/// What the service counted so far.
pub fn snapshot() -> Result<Snapshot, Error> {
    let s = shared()?;
    Ok(Snapshot {
        ticks: s.clock.ticks(),
        signalled: s.signalled.load(Ordering::Relaxed),
        self_clocked: s.self_clocked.load(Ordering::Relaxed),
        events: u32::try_from(
            s.running
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len(),
        )
        .unwrap_or(u32::MAX),
        ticks_per_vsync: s.rates.ticks_per_vsync(),
        hold: s.hold.load(Ordering::Relaxed),
        cap: s.cap.load(Ordering::Relaxed),
    })
}

fn worker(s: Arc<Shared>, handle: u32) {
    let mut seen = s.clock.ticks();
    loop {
        if wait_vsync(&s.clock, &s.rates, &mut seen) == Wait::SelfClocked {
            s.self_clocked.fetch_add(1, Ordering::Relaxed);
        }
        let signalled = kernel().is_some_and(|k| k.signal_event(handle).is_ok());
        if !signalled {
            break;
        }
        s.signalled.fetch_add(1, Ordering::Relaxed);
    }
    s.running
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|h| *h != handle);
}

/// `nn::vi::GetDisplayVsyncEvent(SystemEventType *event, Display *display)`: binds the event and signals it from the
/// ticks of the display link. A second request for an event that already runs starts no second thread, so the rate does
/// not double.
extern "C" fn get_display_vsync_event(event: u64, _display: u64) -> u32 {
    let Ok(s) = shared() else {
        return RES_FAIL;
    };
    let Ok(handle) = (s.bind)(event) else {
        return RES_FAIL;
    };
    {
        let mut running = s.running.lock().unwrap_or_else(PoisonError::into_inner);
        if running.contains(&handle) {
            return 0;
        }
        running.push(handle);
    }
    let s2 = s.clone();
    let started = thread::Builder::new()
        .name("nxkit-vsync".into())
        .spawn(move || worker(s2, handle));
    if started.is_err() {
        s.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|h| *h != handle);
        return RES_FAIL;
    }
    0
}

/// The SDK-level functions of the service, by Switch mangled name (R14).
pub fn exports() -> Vec<Export> {
    vec![Export::new(
        VSYNC_EVENT,
        get_display_vsync_event as extern "C" fn(u64, u64) -> u32 as usize,
    )]
}

/// The IPC services of the service. There are none, because the game reaches the vsync event through the SDK function
/// above. The `vi:u` IPC interface that the SDK function wraps is not served.
pub fn services() -> Vec<Named> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_has_the_vsync_export_and_no_ipc() {
        let e = exports();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].name, VSYNC_EVENT);
        assert_ne!(e[0].addr, 0);
        assert!(services().is_empty());
    }

    #[test]
    fn a_call_before_init_is_an_error_and_the_export_fails() {
        // Another test of this binary may have run init. Only the failing paths are checked here.
        if SHARED.get().is_none() {
            assert_eq!(on_display_tick(), Err(Error::NotInitialised));
            assert_eq!(get_display_vsync_event(0x1000, 0), RES_FAIL);
        }
    }

    #[test]
    fn a_bad_config_is_refused() {
        fn bind(_: u64) -> Result<u32, u32> {
            Ok(1)
        }
        let c = Config {
            bind_system_event: bind,
            state_page: None,
            display_hz: 0.0,
            vsync_hz: 60.0,
            step: StepMode::Own,
            limits: None,
            display_mode: false,
        };
        assert_eq!(init(&c), Err(Error::BadRate));
        let c = Config {
            display_hz: 120.0,
            step: StepMode::Variable(120),
            ..c
        };
        assert_eq!(init(&c), Err(Error::NoStatePage));
    }
}
