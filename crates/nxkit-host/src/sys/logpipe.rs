//! The log of the host without blocking a game thread.
//!
//! Every part of the runtime writes its log to stderr with `eprintln!`, on the thread that has something to say: the
//! game's render thread in the present, its audio thread, a worker in a svc. A write to a file blocks while the disk
//! is busy, and on a busy external disk one write took seconds (measured on Octopath Traveler II: the render thread
//! spent 0.4 s of a second in `write` to a log on a USB disk). Unreal Engine's hang detector ends the game after 5 s
//! without a present (`FThreadHeartBeat::OnPresentHang`, a deliberate write to address 3).
//!
//! [`install`] puts a pipe in place of fd 2 (and of fd 1 when it is the same file). One thread reads the pipe into
//! memory at once, another writes the memory to the original file. A game thread then only waits for a copy into the
//! pipe. The memory holds at most `MAX_QUEUED` bytes; what comes beyond is dropped and counted. At the exit of the
//! process [`flush`] puts the original file back and writes what is left. A crash report writes to the original file
//! directly ([`direct_fd`]), because the writer thread does not run after a fatal signal.

use std::collections::VecDeque;
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

unsafe extern "C" {
    fn pipe(fds: *mut c_int) -> c_int;
    fn dup(fd: c_int) -> c_int;
    fn dup2(from: c_int, to: c_int) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn read(fd: c_int, buf: *mut c_void, n: usize) -> isize;
    fn write(fd: c_int, buf: *const c_void, n: usize) -> isize;
    fn fstat(fd: c_int, st: *mut Stat) -> c_int;
    fn atexit(callback: extern "C" fn()) -> c_int;
}

/// The start of `struct stat` of macOS (64-bit inodes): `st_dev`, `st_mode`, `st_nlink`, `st_ino`, then fields the
/// host does not read.
#[repr(C)]
struct Stat {
    dev: i32,
    mode: u16,
    nlink: u16,
    ino: u64,
    rest: [u64; 16],
}

/// The bytes the memory holds at most before the log drops lines.
const MAX_QUEUED: usize = 64 << 20;

struct Log {
    queue: Mutex<Queue>,
    ready: Condvar,
    /// The original stderr, or -1 once [`flush`] put it back.
    original: AtomicI32,
    /// The bytes in memory that the writer has not written yet, for [`settle`] (no lock in a signal handler).
    queued: AtomicU64,
    /// The bytes dropped because the memory was full.
    dropped: AtomicU64,
}

#[derive(Default)]
struct Queue {
    chunks: VecDeque<Vec<u8>>,
    bytes: usize,
    /// The reader saw the end of the pipe.
    closed: bool,
    /// The writer wrote everything up to the end.
    done: bool,
}

/// The log, once [`install`] made the pipe.
// Process-wide (W26): the exit handler (`atexit`) and the crash handler (a signal handler) have no context.
static LOG: OnceLock<Log> = OnceLock::new();

fn original() -> c_int {
    LOG.get().map_or(-1, |l| l.original.load(Ordering::Relaxed))
}

fn lock(m: &Mutex<Queue>) -> std::sync::MutexGuard<'_, Queue> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The file a crash report writes to: the original stderr when the log goes through the pipe, else 2.
pub fn direct_fd() -> c_int {
    match original() {
        fd if fd >= 0 => fd,
        _ => 2,
    }
}

/// Waits up to `ms` milliseconds until the writer wrote what is in memory. The crash handler calls it before its
/// report, so that the lines before the crash come first. It takes no lock and allocates nothing.
pub fn settle(ms: u32) {
    let Some(log) = LOG.get() else {
        return;
    };
    if log.original.load(Ordering::Relaxed) < 0 {
        return;
    }
    for _ in 0..ms {
        if log.queued.load(Ordering::Relaxed) == 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn same_file(a: c_int, b: c_int) -> bool {
    let mut sa = Stat {
        dev: 0,
        mode: 0,
        nlink: 0,
        ino: 0,
        rest: [0; 16],
    };
    let mut sb = Stat { ..sa };
    // SAFETY: both point to a `Stat` that is larger than the start of `struct stat` it describes plus the rest the
    // kernel writes (144 bytes on macOS).
    let ok = unsafe { fstat(a, &mut sa) == 0 && fstat(b, &mut sb) == 0 };
    ok && sa.dev == sb.dev && sa.ino == sb.ino
}

fn write_all(fd: c_int, mut b: &[u8]) {
    while !b.is_empty() {
        // SAFETY: `b` is a live slice.
        let n = unsafe { write(fd, b.as_ptr().cast(), b.len()) };
        if n <= 0 {
            return;
        }
        b = &b[n as usize..];
    }
}

fn reader(fd: c_int, log: &'static Log) {
    let mut buf = vec![0u8; 64 << 10];
    loop {
        // SAFETY: `buf` is writable for its length.
        let n = unsafe { read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            break;
        }
        let n = n as usize;
        let mut q = lock(&log.queue);
        if q.bytes + n > MAX_QUEUED {
            log.dropped.fetch_add(n as u64, Ordering::Relaxed);
            continue;
        }
        q.bytes += n;
        log.queued.fetch_add(n as u64, Ordering::Relaxed);
        q.chunks.push_back(buf[..n].to_vec());
        drop(q);
        log.ready.notify_all();
    }
    lock(&log.queue).closed = true;
    log.ready.notify_all();
}

fn writer(fd: c_int, log: &'static Log) {
    loop {
        let mut q = lock(&log.queue);
        while q.chunks.is_empty() && !q.closed {
            q = log.ready.wait(q).unwrap_or_else(|e| e.into_inner());
        }
        let Some(chunk) = q.chunks.pop_front() else {
            q.done = true;
            drop(q);
            log.ready.notify_all();
            return;
        };
        q.bytes -= chunk.len();
        drop(q);
        write_all(fd, &chunk);
        log.queued.fetch_sub(chunk.len() as u64, Ordering::Relaxed);
    }
}

/// Puts the pipe in place of fd 2, and of fd 1 when it is the same file. It does nothing when a step fails, and then
/// the log blocks as before.
pub fn install() {
    // SAFETY: plain descriptor calls; `fds` has room for two descriptors.
    unsafe {
        let original = dup(2);
        if original < 0 {
            return;
        }
        let mut fds = [0 as c_int; 2];
        if pipe(fds.as_mut_ptr()) != 0 {
            close(original);
            return;
        }
        if LOG.get().is_some() {
            close(original);
            close(fds[0]);
            close(fds[1]);
            return;
        }
        let log = LOG.get_or_init(|| Log {
            queue: Mutex::new(Queue::default()),
            ready: Condvar::new(),
            original: AtomicI32::new(-1),
            queued: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        });
        let (r, w) = (fds[0], fds[1]);
        let started = std::thread::Builder::new()
            .name("nxkit-log-read".into())
            .spawn(move || reader(r, log))
            .and_then(|_| {
                std::thread::Builder::new()
                    .name("nxkit-log-write".into())
                    .spawn(move || writer(original, log))
            });
        if started.is_err() {
            close(w);
            return;
        }
        let both = same_file(1, 2);
        log.original.store(original, Ordering::Relaxed);
        dup2(w, 2);
        if both {
            dup2(w, 1);
        }
        close(w);
        atexit(at_exit);
    }
}

extern "C" fn at_exit() {
    flush();
}

/// Puts the original file back in place of the pipe and waits up to two seconds until the writer wrote what was in
/// the pipe and in memory.
pub fn flush() {
    let Some(log) = LOG.get() else {
        return;
    };
    let original = log.original.swap(-1, Ordering::Relaxed);
    if original < 0 {
        return;
    }
    // SAFETY: plain descriptor calls. Once fd 1 and fd 2 are the original file, the pipe has no writer left and the
    // reader sees its end.
    unsafe {
        if same_file(1, 2) {
            dup2(original, 1);
        }
        dup2(original, 2);
    }
    let q = lock(&log.queue);
    let (q, _) = log
        .ready
        .wait_timeout_while(q, Duration::from_secs(2), |q| !q.done)
        .unwrap_or_else(|e| e.into_inner());
    drop(q);
    let dropped = log.dropped.load(Ordering::Relaxed);
    if dropped > 0 {
        eprintln!(
            "nxkit-host: the log dropped {dropped} bytes while the file was slower than the game"
        );
    }
}
