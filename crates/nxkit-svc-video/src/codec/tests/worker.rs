//! Tests of the worker thread of a stream, with a mock backend.

use super::*;

use std::time::{Duration, Instant};

/// Lags like rav1d with frame threads: a picture comes out when `MOCK_LAG` more packets were pushed, or when it
/// is waited for.
const MOCK_LAG: usize = 3;

struct MockPipe {
    /// Two pictures come out of every third packet, and every seventh packet shows nothing.
    odd: bool,
    pushed: u32,
    inside: VecDeque<Pic>,
    /// Where the pictures of `push` and `wait` came from, to see that the worker waits.
    waits: Arc<AtomicU32>,
}

impl Pipeline for MockPipe {
    fn lag(&self) -> usize {
        MOCK_LAG
    }

    fn push(&mut self, _packet: &[u8], ts: i64, out: &mut Vec<Pic>) -> Result<bool, String> {
        // The time that a frame takes.
        std::thread::sleep(Duration::from_millis(2));
        self.pushed += 1;
        if self.odd && self.pushed.is_multiple_of(7) {
            return Ok(true);
        }
        self.inside.push_back((Arc::new(grey()), ts));
        if self.inside.len() > MOCK_LAG {
            out.extend(self.inside.pop_front());
            if self.odd && self.pushed.is_multiple_of(3) {
                out.extend(self.inside.pop_front());
            }
        }
        Ok(true)
    }

    fn wait(&mut self, out: &mut Vec<Pic>) -> Result<(), String> {
        self.waits.fetch_add(1, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(2));
        out.extend(self.inside.pop_front());
        Ok(())
    }

    fn flush(&mut self) {
        self.inside.clear();
    }
}

struct MockBackend {
    odd: bool,
}

impl Backend for MockBackend {
    fn decode(&mut self, _packet: &[u8]) -> Result<Option<Arc<Frame>>, String> {
        Ok(None)
    }

    fn pipeline(&mut self, _packet: &[u8]) -> Option<Box<dyn Pipeline>> {
        Some(Box::new(MockPipe {
            odd: self.odd,
            pushed: 0,
            inside: VecDeque::new(),
            waits: Arc::new(AtomicU32::new(0)),
        }))
    }
}

fn open_mock() -> Result<Box<dyn Backend>, String> {
    Ok(Box::new(MockBackend { odd: false }))
}

fn open_odd() -> Result<Box<dyn Backend>, String> {
    Ok(Box::new(MockBackend { odd: true }))
}

/// The effects that the worker and the calls carried out.
#[derive(Default)]
struct Record {
    events: Vec<bool>,
    filled: Vec<(u32, u32)>,
}

struct Rig {
    dec: Arc<Mutex<Dec>>,
    rec: Arc<Mutex<Record>>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        stop_worker(&self.dec);
    }
}

fn rig() -> Rig {
    let rec = Arc::new(Mutex::new(Record::default()));
    let dec = Arc::new(Mutex::new(Dec::new(1, Codec::Av1, 0x1000)));
    {
        let mut d = lock(&dec);
        d.me = Arc::downgrade(&dec);
        d.open_av1 = open_mock;
        let r = Arc::clone(&rec);
        d.applier = Arc::new(move |_, _, fx| {
            let mut r = lock(&r);
            r.events.extend(fx.event);
            r.filled.extend(fx.filled);
        });
    }
    Rig { dec, rec }
}

impl Rig {
    fn call<R>(&self, f: impl FnOnce(&mut Dec, &mut Effects) -> R) -> R {
        let mut fx = Effects::default();
        let mut d = lock(&self.dec);
        let r = f(&mut d, &mut fx);
        deliver(d, fx);
        r
    }

    fn submit(&self, ts: i64, eos: bool) -> Result<u64, u32> {
        self.call(|d, fx| d.submit(&[1, 2, 3], ts, eos, fx))
    }

    /// Registers `n` buffers and releases them, so that pictures can go in.
    fn buffers(&self, n: u32) {
        for i in 0..n {
            self.call(|d, fx| {
                d.register(i, None, i);
                d.release(i, fx);
            });
        }
    }

    fn properties(&self, i: u32) -> Result<(u16, u16, i64, bool), u32> {
        lock(&self.dec).properties(i)
    }

    fn wait_for(&self, what: &str, mut ok: impl FnMut(&Rig) -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        while !ok(self) {
            assert!(Instant::now() < end, "timed out: {what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn state(&self) -> String {
        let d = lock(&self.dec);
        format!(
            "held {} queued {} inflight {} busy {} event {:?}",
            d.held.len(),
            d.queued.len(),
            d.inflight,
            d.busy,
            d.event_state
        )
    }

    /// The game takes the filled buffers and releases them again.
    fn take_all(&self) {
        for i in 0..8 {
            if self.properties(i).is_ok() {
                self.call(|d, fx| d.release(i, fx));
            }
        }
    }

    fn reset(&self) {
        let mut fx = Effects::default();
        let mut d = lock(&self.dec);
        d.reset(&mut fx);
        let d = Dec::wait_idle(d);
        deliver(d, fx);
    }
}

#[test]
fn pictures_arrive_without_further_submits() {
    // The starvation case: the decoder keeps MOCK_LAG pictures until the next push, and the game sends the next
    // packet only when it has a picture.
    let r = rig();
    r.buffers(8);
    for ts in 0..3 {
        assert!(r.submit(ts, false).is_ok());
    }
    r.wait_for("three pictures", |r| {
        (0..3).all(|i| r.properties(i).is_ok())
    });
    for i in 0..3 {
        assert_eq!(r.properties(i).map(|p| p.2), Ok(i64::from(i)));
    }
    assert!(lock(&r.rec).events.contains(&true));
    assert_eq!(lock(&r.dec).load(), 0);
}

#[test]
fn pictures_keep_their_order_and_timestamps() {
    let r = rig();
    r.buffers(8);
    let mut seen = Vec::new();
    let mut sent = 0;
    let end = Instant::now() + Duration::from_secs(10);
    while seen.len() < 60 {
        assert!(Instant::now() < end, "timed out with {seen:?}");
        if sent < 60 && r.submit(1000 + sent, false).is_ok() {
            sent += 1;
        }
        // Take the filled buffers in the order of their release, and release them again.
        for i in 0..8 {
            if let Ok((_, _, ts, _)) = r.properties(i) {
                seen.push(ts);
                r.call(|d, fx| d.release(i, fx));
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let want: Vec<i64> = (1000..1060).collect();
    assert_eq!(seen, want);
}

#[test]
fn the_end_of_stream_drains_the_decoder_and_marks_the_last_picture() {
    let r = rig();
    r.buffers(8);
    assert!(r.submit(10, false).is_ok());
    assert!(r.submit(11, false).is_ok());
    assert!(r.submit(12, true).is_ok());
    r.wait_for("the last picture", |r| r.properties(2).is_ok());
    assert_eq!(r.properties(0).map(|p| p.3), Ok(false));
    assert_eq!(r.properties(1).map(|p| p.3), Ok(false));
    assert_eq!(r.properties(2), Ok((2, 2, 12, true)));
}

#[test]
fn an_end_of_stream_without_a_picture_gets_a_buffer_of_its_own() {
    let r = rig();
    r.buffers(4);
    assert!(r.submit(1, false).is_ok());
    r.wait_for("the picture", |r| r.properties(0).is_ok());
    assert!(r.call(|d, fx| d.submit(&[], 0, true, fx)).is_ok());
    r.wait_for("the end", |r| r.properties(1).is_ok());
    assert_eq!(r.properties(1), Ok((0, 0, 0, true)));
}

#[test]
fn a_flush_during_decode_drops_everything_and_the_worker_goes_on() {
    let r = rig();
    r.buffers(8);
    for ts in 0..6 {
        assert!(r.submit(ts, false).is_ok());
    }
    r.reset();
    {
        let d = lock(&r.dec);
        assert!(!d.busy && !d.flush_req);
        assert_eq!((d.load(), d.queued.len(), d.inflight), (0, 0, 0));
    }
    // No picture of the old stream shows up later.
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(lock(&r.dec).load(), 0);
    let before = (0..8).filter(|&i| r.properties(i).is_ok()).count();
    assert!(before < 6);
    // The stream starts over.
    for i in 0..8 {
        r.call(|d, fx| d.release(i, fx));
    }
    assert!(r.submit(500, false).is_ok());
    r.wait_for("the new picture", |r| {
        (0..8).any(|i| r.properties(i).is_ok_and(|p| p.2 == 500))
    });
}

#[test]
fn hold_counts_packets_in_flight_and_pictures() {
    let r = rig();
    // No buffer is released: nothing leaves `held`.
    let mut accepted = 0;
    let mut busy = None;
    for ts in 0..HOLD as i64 + 4 {
        let mut fx = Effects::default();
        let res = lock(&r.dec).submit(&[1], ts, false, &mut fx);
        match res {
            Ok(_) => accepted += 1,
            Err(e) => {
                busy = Some((e, fx.event));
                break;
            }
        }
        r.call(|_, _| ());
    }
    assert_eq!(accepted, HOLD);
    assert_eq!(busy, Some((BUSY, Some(false))));
    {
        let d = lock(&r.dec);
        assert_eq!(d.load(), HOLD);
    }
    // Wait for the worker to finish what it has: the load does not change by that.
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(lock(&r.dec).load(), HOLD);
    // Room comes back with released buffers. The event stays down until the load is under the limit.
    lock(&r.rec).events.clear();
    let limit = lock(&r.dec).ready_limit();
    assert!(limit < HOLD);
    for i in 0..(HOLD - limit + 1) as u32 {
        r.call(|d, fx| {
            d.register(i, None, i);
            d.release(i, fx);
        });
    }
    assert_eq!(lock(&r.dec).load(), limit - 1);
    let events = lock(&r.rec).events.clone();
    assert_eq!(events.last(), Some(&true));
    assert!(events[..events.len() - 1].iter().all(|e| !e));
}

#[test]
fn a_player_that_sends_on_the_event_never_stalls() {
    // Sends a packet only when the event is up, as a player does, and takes pictures as they come.
    let r = rig();
    r.buffers(8);
    let mut got = Vec::new();
    let mut sent = 0;
    let mut up = true;
    let mut seen_events = 0;
    let end = Instant::now() + Duration::from_secs(10);
    while got.len() < 40 {
        assert!(
            Instant::now() < end,
            "stalled: sent {sent}, got {}",
            got.len()
        );
        {
            let ev = lock(&r.rec);
            for &e in &ev.events[seen_events..] {
                up = e;
            }
            seen_events = ev.events.len();
        }
        if up && sent < 40 {
            let mut fx = Effects::default();
            let res = lock(&r.dec).submit(&[1], sent, false, &mut fx);
            if res.is_ok() {
                sent += 1;
            }
            // The effect of the call, as `deliver` would.
            let mut rec = lock(&r.rec);
            rec.events.extend(fx.event);
            rec.filled.extend(fx.filled);
        }
        for i in 0..8 {
            if let Ok((_, _, ts, _)) = r.properties(i) {
                got.push(ts);
                r.call(|d, fx| d.release(i, fx));
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(got, (0..40).collect::<Vec<i64>>());
}

#[test]
fn the_load_does_not_stick_with_packets_that_show_nothing_and_a_looping_stream() {
    let r = rig();
    lock(&r.dec).open_av1 = open_odd;
    r.buffers(8);
    let mut ts = 0;
    for round in 0..4 {
        // A stream of packets, the last one the end of the stream.
        let mut sent = 0;
        let end = Instant::now() + Duration::from_secs(10);
        while sent < 17 {
            assert!(Instant::now() < end, "round {round}: stuck, {}", r.state());
            if r.submit(ts, round == 3 && sent == 16).is_ok() {
                sent += 1;
                ts += 1;
            }
            r.take_all();
        }
        // Nothing more is sent: the game takes what comes, and the load goes to nothing.
        let end = Instant::now() + Duration::from_secs(10);
        while lock(&r.dec).load() > 0 {
            assert!(
                Instant::now() < end,
                "round {round}: load stuck, {}",
                r.state()
            );
            r.take_all();
            std::thread::sleep(Duration::from_millis(1));
        }
        // The stream ends and a new one starts, as a loop of the movie does.
        r.take_all();
        let d = lock(&r.dec);
        assert_eq!(d.inflight, 0, "round {round}");
        assert!(d.queued.is_empty() && d.held.is_empty());
    }
    // The event is up again, so that the game sends.
    assert_eq!(lock(&r.dec).event_state, Some(true));
}

#[test]
fn stopping_the_worker_joins_it() {
    let r = rig();
    r.buffers(2);
    assert!(r.submit(1, false).is_ok());
    stop_worker(&r.dec);
    assert!(lock(&r.dec).worker.is_none());
}
