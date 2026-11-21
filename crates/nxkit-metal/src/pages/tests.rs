//! Tests of the page tracker, and their helpers.

use super::runs::set_blocks;
use super::*;
use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::sync::Mutex as StdMutex;

/// One test at a time: the tracker is one per process.
static TURN: StdMutex<()> = StdMutex::new(());
const P: u64 = 0x4000;

struct Mem {
    base: *mut u8,
    layout: Layout,
}

impl Mem {
    fn new(pages: u64) -> Self {
        let layout = Layout::from_size_align((pages * P) as usize, P as usize).unwrap();
        // SAFETY: the layout is not empty.
        let base = unsafe { alloc_zeroed(layout) };
        assert!(!base.is_null());
        Self { base, layout }
    }

    fn addr(&self, page: u64) -> u64 {
        self.base as u64 + page * P
    }

    fn store(&self, page: u64, off: u64, v: u8) {
        // SAFETY: inside the allocation.
        unsafe {
            self.base.add((page * P + off) as usize).write_volatile(v);
        }
    }

    fn load(&self, page: u64, off: u64) -> u8 {
        // SAFETY: inside the allocation.
        unsafe { self.base.add((page * P + off) as usize).read_volatile() }
    }
}

impl Drop for Mem {
    fn drop(&mut self) {
        // SAFETY: the allocation came from `alloc_zeroed` with this layout; make it writable first.
        unsafe {
            mprotect(self.base.cast(), self.layout.size(), PROT_RW);
            dealloc(self.base, self.layout);
        }
    }
}

fn setup(pages: u64) -> (Mem, &'static Pages) {
    let m = Mem::new(pages);
    let pg = Pages::global();
    pg.attach(m.addr(0), pages * P);
    (m, pg)
}

mod emulate;
mod fallback;
mod late;
mod late_writes;
mod mirrored;
mod tracking;

/// Collects the staged pages of a test flush.
#[derive(Default)]
struct Collect {
    pages: Vec<(u32, Vec<u8>)>,
    skips: Vec<(u32, [u64; 4])>,
}

impl Stager for Collect {
    fn begin(&mut self, _max: usize) {}

    fn stage(&mut self, page: u32, bytes: &[u8]) {
        self.stage_masked(page, bytes, [0; 4]);
    }

    fn stage_masked(&mut self, page: u32, bytes: &[u8], skip: [u64; 4]) {
        self.pages.retain(|p| p.0 != page);
        self.skips.retain(|p| p.0 != page);
        self.pages.push((page, bytes.to_vec()));
        self.skips.push((page, skip));
    }
}

fn mirrored(pages: u64) -> (Mem, Mem, &'static Pages) {
    let (m, pg) = setup(pages);
    let mir = Mem::new(pages);
    pg.set_mirror(mir.addr(0));
    (m, mir, pg)
}

/// An anonymous mapping (the alias needs real VM regions, not heap blocks).
struct Map {
    base: *mut u8,
    len: usize,
}

impl Map {
    fn new(pages: u64) -> Self {
        let len = (pages * P) as usize;
        // SAFETY: an anonymous private mapping; the kernel picks the address.
        let base = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                PROT_RW,
                MAP_PRIVATE | MAP_ANON,
                -1,
                0,
            )
        };
        assert!(base as isize != -1);
        Self {
            base: base.cast(),
            len,
        }
    }

    fn addr(&self, page: u64) -> u64 {
        self.base as u64 + page * P
    }

    fn store(&self, page: u64, off: u64, v: u8) {
        // SAFETY: inside the mapping.
        unsafe { self.base.add((page * P + off) as usize).write_volatile(v) }
    }

    fn load(&self, page: u64, off: u64) -> u8 {
        // SAFETY: inside the mapping.
        unsafe { self.base.add((page * P + off) as usize).read_volatile() }
    }
}

impl Drop for Map {
    fn drop(&mut self) {
        // SAFETY: the mapping came from `mmap`; make it writable first so nothing is left protected.
        unsafe {
            mprotect(self.base.cast(), self.len, PROT_RW);
            munmap(self.base.cast(), self.len);
        }
    }
}

/// A tracker over a mapped guest and a mapped mirror, with the alias.
fn emulating(pages: u64) -> (Map, Map, &'static Pages) {
    let m = Map::new(pages);
    let mir = Map::new(pages);
    let pg = Pages::global();
    pg.attach(m.addr(0), pages * P);
    pg.set_mirror(mir.addr(0));
    pg.make_alias().expect("alias");
    pg.set_emulation(true);
    (m, mir, pg)
}

fn pat(i: u64) -> u8 {
    (i * 7 + 3) as u8
}

/// A page in step 1 of the late-results tests: frame 1 wrote the bytes `[64, 192)` of page 1, the GPU's result of it is
/// `a1` at byte 70 and `a2` at byte 130, the frame completed, and a read copied the page (so the page is a CPU-read GPU
/// result and guest memory holds frame 1).
fn late_page(a1: u8, a2: u8) -> (Map, Map, &'static Pages) {
    let (m, mir, pg) = emulating(4);
    pg.set_late(true);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(1, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    mir.store(1, 70, a1);
    mir.store(1, 130, a2);
    pg.complete(1);
    assert_eq!(
        m.load(1, 70),
        a1,
        "the first read of a result waits for nothing (the frame is done) and copies the page"
    );
    assert_eq!(m.load(1, 130), a2);
    (m, mir, pg)
}

/// What the encoder and the GPU do for the late pages of a frame (`Pages::late_copy_pages`): a buffer with a copy
/// of each page from the mirror as the frame starts ([`late_start`]) and as it ends ([`late_end`]).
struct Copies {
    buf: Vec<u8>,
    pages: Vec<u32>,
}

fn late_start(pg: &Pages, mir: &Map, f: u64) -> Copies {
    let pages = pg.late_copy_pages(f);
    assert!(!pages.is_empty(), "frame {f} has late pages");
    let n = pages.len();
    let mut buf = vec![0u8; 2 * n * P as usize];
    for (i, &p) in pages.iter().enumerate() {
        for o in 0..P {
            buf[i * P as usize + o as usize] = mir.load(u64::from(p), o);
        }
    }
    let base = buf.as_ptr() as u64;
    pg.set_late_copies(f, base, base + n as u64 * P);
    Copies { buf, pages }
}

fn late_end(c: &mut Copies, mir: &Map) {
    let n = c.pages.len();
    for (i, &p) in c.pages.iter().enumerate() {
        for o in 0..P {
            c.buf[(n + i) * P as usize + o as usize] = mir.load(u64::from(p), o);
        }
    }
}

fn done(pg: &Pages) {
    pg.set_late(false);
    pg.set_adaptive(false, false);
    pg.set_mirror(0);
}

/// Waits until the copy-back of the completed frame has opened the page.
fn wait_clean(pg: &Pages, m: &Map) {
    for _ in 0..400 {
        if pg.state_of(m.addr(1)) == "clean" {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the copy-back did not run: {}", pg.state_of(m.addr(1)));
}
