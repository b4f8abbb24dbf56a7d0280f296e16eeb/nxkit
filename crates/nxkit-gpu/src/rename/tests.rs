//! Tests of the renamer, with the guest and state helpers of the test files.

use super::*;
use std::cell::RefCell;

mod keys;
mod learn;
mod masks;
mod state;

fn words(w: &[u32]) -> Vec<u8> {
    w.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn frame(l: &mut Learn, v: Verdict) -> Option<Class> {
    l.tick();
    l.observe(v)
}

fn renamed() -> Learn {
    let mut l = Learn::new();
    for _ in 0..LEARN_FRAMES {
        frame(&mut l, Verdict::Clean);
    }
    assert_eq!(l.class(), Class::Rename);
    l
}

/// A guest memory for the state machine: bytes by address.
struct Guest(RefCell<Vec<u8>>);

impl Guest {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        let m = self.0.borrow();
        let a = (addr - BASE) as usize;
        match m.get(a..a + buf.len()) {
            Some(s) => {
                buf.copy_from_slice(s);
                true
            }
            None => false,
        }
    }

    fn set(&self, addr: u64, w: &[u32]) {
        let mut m = self.0.borrow_mut();
        let a = (addr - BASE) as usize;
        m[a..a + 4 * w.len()].copy_from_slice(&words(w));
    }
}

const BASE: u64 = 0x1000;
const SUM: u64 = BASE + 0x100;

fn state_with_sum() -> State {
    let mut st = State::default();
    st.candidate(SUM, 0x40);
    st
}

/// One game frame as the host sees it: the CPU resets the word, the frame is flushed, the GPU adds `sum` into it and the
/// wait reads the bytes after the frame.
fn play(st: &mut State, g: &Guest, fence: u64, sum: u32, reset: bool) -> Flushed {
    if reset {
        g.set(SUM, &[0]);
    }
    let bound = [(SUM, 0x40)];
    let f = st.flush(
        fence,
        &bound,
        &|r| vec![false; r.len()],
        &|| Vec::new(),
        &|a, b| g.read(a, b),
    );
    // The GPU adds into the guest word when the frame is not renamed.
    if f.renamed.is_empty() {
        let mut w = [0u8; 4];
        g.read(SUM, &mut w);
        g.set(SUM, &[u32::from_le_bytes(w) + sum]);
    }
    st.waited(fence, fence, &|a, b| g.read(a, b));
    f
}

/// One judged frame of a read-only range: the flush, the CPU writing a word in flight, the wait.
fn judged_frame(st: &mut State, g: &Guest, fence: u64, write: bool) -> Flushed {
    let f = st.flush(
        fence,
        &[(SUM, 0x40)],
        &|r| vec![false; r.len()],
        &|| Vec::new(),
        &|a, b| g.read(a, b),
    );
    if write {
        g.set(SUM + 4, &[fence as u32]);
    }
    st.before_wait(fence, &|a, b| g.read(a, b));
    st.waited(fence, fence, &|a, b| g.read(a, b));
    f
}

fn run_to(st: &mut State, g: &Guest, last: u64) {
    for fence in st.frame + 1..=last {
        st.flush(
            fence,
            &[(SUM, 0x40)],
            &|r| vec![false; r.len()],
            &|| Vec::new(),
            &|a, b| g.read(a, b),
        );
    }
}

/// `cpu_word` of a judged frame in which the CPU writes nothing.
const NO_CPU: u32 = u32::MAX;

/// A judged frame of a written range, as the host sees it: the flush gives the range a copy (a probe); the CPU writes
/// `cpu_word` after it; the wait begins (`landed` says whether the GPU's word has already gone back into guest memory);
/// then the GPU is done with a copy that has `gpu_word` changed.
fn judged_rw(
    st: &mut State,
    g: &Guest,
    fence: u64,
    cpu_word: u32,
    gpu_word: u32,
    landed: bool,
) -> Flushed {
    let f = st.flush(
        fence,
        &[(SUM, 0x40)],
        &|r| vec![false; r.len()],
        &|| Vec::new(),
        &|a, b| g.read(a, b),
    );
    assert_eq!(
        f.renamed.len(),
        1,
        "the range gets a copy in the judged frame"
    );
    assert_eq!(f.renamed[0].kind, Kind::Probe);
    assert!(
        f.renamed[0].cmask.iter().all(|w| *w == 0),
        "no CPU word yet"
    );
    assert!(f.sampling);
    let mut start = vec![0u8; f.renamed[0].len as usize];
    g.read(SUM, &mut start);
    let mut end = start.clone();
    let at = 4 * gpu_word as usize;
    end[at..at + 4].copy_from_slice(&(fence as u32 + 1000).to_le_bytes());
    if cpu_word != NO_CPU {
        g.set(SUM + 4 * u64::from(cpu_word), &[fence as u32]);
    }
    if landed {
        g.set(SUM + 4 * u64::from(gpu_word), &[fence as u32 + 1000]);
    }
    st.before_wait(fence, &|a, b| g.read(a, b));
    st.probed(fence, SUM, &end, &start);
    f
}

fn never_range() -> State {
    let mut st = State::default();
    st.candidate(SUM, 0x40);
    st.cands.get_mut(&SUM).unwrap().learn.set(Class::Never);
    st
}

fn key(prog: u64, slot: u32, size: u32) -> Key {
    Key {
        prog,
        slot_stage: slot,
        size: size.next_multiple_of(256),
    }
}

fn binding(addr: u64, size: u32, k: Key) -> Binding {
    Binding {
        addr,
        size,
        key: k,
        at: (0, 0, 0),
    }
}

/// A state that has learned from the range at `SUM` that the binding `k` is a merge range.
fn taught(k: Key) -> State {
    let mut st = State::default();
    st.candidate(SUM, 0x40);
    {
        let c = st.cands.get_mut(&SUM).unwrap();
        c.learn.set(Class::Merge);
        c.hits = SNAPSHOT_HITS;
        c.cmask = vec![0b0010];
        c.gmask = vec![0b0100];
    }
    st.adopt(&[binding(SUM, 0x40, k)]);
    st.push_keys(SUM);
    st
}
