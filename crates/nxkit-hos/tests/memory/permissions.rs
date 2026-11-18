//! Host-page projection, partial host failures and rollback.
#![allow(clippy::unwrap_used, reason = "test state must not be poisoned")]
use super::*;
use std::collections::BTreeMap;

#[derive(Default)]
struct State {
    calls: Vec<(u64, u64, u32)>,
    pages: BTreeMap<u64, u32>,
    failures: Vec<usize>,
    partial: Vec<usize>,
}

struct Pages {
    mem: Arc<FakeMemory>,
    state: Mutex<State>,
}

impl HostSpace for Pages {
    fn page_size(&self) -> u64 {
        0x4000
    }
    fn protect(&self, a: u64, n: u64, p: u32) -> bool {
        let mut state = self.state.lock().unwrap();
        state.calls.push((a, n, p));
        let fail = state.failures.contains(&state.calls.len());
        // A failed host call may have changed a prefix of a multi-page span.
        let changed = if fail && state.partial.contains(&state.calls.len()) {
            n.min(0x4000)
        } else if fail {
            0
        } else {
            n
        };
        for at in (a..a + changed).step_by(0x4000) {
            state.pages.insert(at, p);
        }
        !fail
    }
    fn release(&self, a: u64, n: u64) -> bool {
        self.mem.release(a, n)
    }
    fn zero(&self, a: u64, n: u64) -> bool {
        self.mem.zero(a, n)
    }
    fn remap(&self, d: u64, s: u64, n: u64, p: u32) -> bool {
        self.mem.remap(d, s, n, p)
    }
    fn copy(&self, d: u64, s: u64, n: u64) -> bool {
        self.mem.copy(d, s, n)
    }
}

fn pages(config: Config) -> (Rig, Arc<Pages>) {
    let mem = Arc::new(FakeMemory::new(BASE, (SIZE / 4) as usize));
    let host = Arc::new(Pages {
        mem: mem.clone(),
        state: Mutex::new(State::default()),
    });
    let r = code_data::build_config(host.clone(), mem.clone(), mem, Vec::new(), config);
    assert_eq!(r.svc(0x01, &[0, MIB2])[0], 0);
    host.state.lock().unwrap().calls.clear();
    (r, host)
}

#[test]
fn eight_kib_relro_preserves_writable_neighbour_on_sixteen_kib_host() {
    let (r, host) = pages(Config::default());
    assert_eq!(r.svc(0x02, &[HEAP, 0x2000, 1])[0], 0);
    assert_eq!(r.query(HEAP).4, 1);
    assert_eq!(r.query(HEAP + 0x2000).4, 3);
    assert_eq!(host.state.lock().unwrap().pages[&HEAP], 3);
    assert_eq!(r.svc(0x02, &[HEAP + 0x2000, 0x2000, 1])[0], 0);
    assert_eq!(host.state.lock().unwrap().pages[&HEAP], 1);
}

#[test]
fn partial_failure_restores_entire_failing_span_and_keeps_guest_permissions() {
    let (r, host) = pages(Config::default());
    {
        let mut state = host.state.lock().unwrap();
        state.failures = vec![2];
        state.partial = vec![2];
    }
    assert_ne!(r.svc(0x02, &[HEAP + 0x1000, 0x10000, 1])[0], 0);
    for offset in (0..0x14000).step_by(0x1000) {
        assert_eq!(r.query(HEAP + offset).4, 3);
    }
    let state = host.state.lock().unwrap();
    assert_eq!(
        state.calls,
        vec![
            (HEAP, 0x4000, 3),
            (HEAP + 0x4000, 0xc000, 1),
            (HEAP, 0x4000, 3),
            (HEAP + 0x4000, 0x4000, 3),
            (HEAP + 0x8000, 0x4000, 3),
            (HEAP + 0xc000, 0x4000, 3),
        ]
    );
    for at in (HEAP..HEAP + 0x14000).step_by(0x4000) {
        assert_eq!(state.pages[&at], 3);
    }
}

#[test]
fn failed_rollback_attempts_remaining_pages_and_requests_process_exit() {
    static EXIT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let (r, host) = pages(Config {
        process_exit: |code| EXIT.store(code, std::sync::atomic::Ordering::Relaxed),
        ..Config::default()
    });
    {
        let mut state = host.state.lock().unwrap();
        state.failures = vec![2, 4];
        state.partial = vec![2];
    }
    assert_ne!(r.svc(0x02, &[HEAP + 0x1000, 0x10000, 1])[0], 0);
    assert_eq!(EXIT.load(std::sync::atomic::Ordering::Relaxed), 42);
    assert_eq!(host.state.lock().unwrap().calls.len(), 6);
    assert_eq!(host.state.lock().unwrap().pages[&(HEAP + 0x4000)], 1);
    assert_eq!(r.query(HEAP + 0x5000).4, 3);
}

#[test]
fn partial_failure_on_the_first_call_restores_the_changed_prefix() {
    let (r, host) = pages(Config::default());
    {
        let mut state = host.state.lock().unwrap();
        state.failures = vec![1];
        state.partial = vec![1];
    }
    assert_ne!(r.svc(0x02, &[HEAP, 0x8000, 1])[0], 0);
    assert_eq!(r.query(HEAP).4, 3);
    let state = host.state.lock().unwrap();
    assert_eq!(state.calls.len(), 3);
    assert_eq!(state.pages[&HEAP], 3);
    assert_eq!(state.pages[&(HEAP + 0x4000)], 3);
}
