//! Tests of the decoder with a mock backend.

use super::*;
use crate::frame::Nv12;
use crate::vp9::key_frame_bytes;
use std::sync::atomic::{AtomicUsize, Ordering};

struct MockIo {
    signals: Mutex<Vec<u64>>,
    jobs: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    defers: AtomicUsize,
}

impl MockIo {
    fn new() -> Arc<MockIo> {
        Arc::new(MockIo {
            signals: Mutex::new(vec![]),
            jobs: Mutex::new(vec![]),
            defers: AtomicUsize::new(0),
        })
    }

    fn run_jobs(&self) {
        let jobs: Vec<_> = std::mem::take(&mut *lock(&self.jobs));
        for j in jobs {
            j();
        }
    }
}

impl Io for MockIo {
    fn defer(&self, _delay: Duration, job: Box<dyn FnOnce() + Send>) {
        self.defers.fetch_add(1, Ordering::SeqCst);
        lock(&self.jobs).push(job);
    }
    fn signal(&self, event: u64) {
        lock(&self.signals).push(event);
    }
    fn event_ok(&self, addr: u64) -> bool {
        addr != 0
    }
    fn read_u64(&self, addr: u64) -> Option<u64> {
        Some(addr)
    }
}

struct MockBackend;

impl Backend for MockBackend {
    fn decode(&mut self, _packet: &[u8]) -> Result<Option<Arc<Frame>>, String> {
        let planes = Nv12 {
            y: vec![0; 16 * 16],
            y_stride: 16,
            uv: vec![128; 16 * 8],
            uv_stride: 16,
        };
        Ok(Some(Arc::new(Frame::from_nv12(16, 16, 3, planes))))
    }
}

fn open(_key: &KeyFrame) -> Result<Box<dyn Backend>, String> {
    Ok(Box::new(MockBackend))
}

fn decoder() -> (Decoder, Arc<MockIo>) {
    let io = MockIo::new();
    let d = Decoder::new(io.clone(), open).unwrap_or_else(|e| unreachable!("{e}"));
    (d, io)
}

fn put(d: &Decoder, index: i32, bytes: &[u8]) {
    let addr = d.input_addr(index).unwrap_or_else(|| unreachable!());
    // SAFETY: the address is inside the input memory of the decoder and the bytes fit in one buffer.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, bytes.len()) };
}

fn wait_output(d: &Decoder) -> (i32, i64, u32, i32) {
    for _ in 0..400 {
        if let Ok(o) = d.acquire_output() {
            return o;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    unreachable!("no output within two seconds")
}

fn send_key(d: &Decoder, pts: i64, flags: u32) -> i32 {
    let (i, _) = d.acquire_input().unwrap_or_else(|e| unreachable!("{e:#x}"));
    let bytes = key_frame_bytes(2, 0, 1920, 1080);
    put(d, i, &bytes);
    assert_eq!(d.send_input(i, 0, bytes.len() as i32, pts, flags), 0);
    i
}

#[test]
fn a_packet_comes_back_as_an_output_with_its_timestamp() {
    let (d, io) = decoder();
    d.register_events(0x100);
    d.start();
    send_key(&d, 1234, 0);
    let (idx, pts, flags, left) = wait_output(&d);
    assert_eq!((pts, flags, left), (1234, 0, 0));
    assert!(d.picture(idx as usize).is_some());
    assert_eq!(
        d.format(),
        Format {
            width: 1920,
            height: 1080,
            colorspace: 3
        }
    );
    io.run_jobs();
    assert!(!lock(&io.signals).is_empty());
}

#[test]
fn nothing_is_handed_out_before_start() {
    let (d, _io) = decoder();
    assert_eq!(d.acquire_input(), Err(ERR_NO_INPUT));
    assert_eq!(d.acquire_output(), Err(ERR_NO_OUTPUT));
}

#[test]
fn an_end_of_stream_packet_without_a_picture_still_gives_an_output() {
    let (d, _io) = decoder();
    d.start();
    let (i, _) = d.acquire_input().unwrap_or_else(|e| unreachable!("{e:#x}"));
    assert_eq!(d.send_input(i, 0, 0, 0, FLAG_EOS), 0);
    let (idx, _, flags, _) = wait_output(&d);
    assert_eq!(flags, FLAG_EOS);
    assert!(d.picture(idx as usize).is_none());
    assert_eq!(d.acquire_input(), Err(ERR_NO_INPUT));
}

#[test]
fn a_stop_drops_the_events_that_were_waiting() {
    let (d, io) = decoder();
    d.register_events(0x100);
    d.start();
    d.stop();
    io.run_jobs();
    assert!(lock(&io.signals).is_empty());
}

#[test]
fn an_input_held_across_a_flush_waits_before_it_comes_again() {
    let (d, _io) = decoder();
    d.start();
    let (i, _) = d.acquire_input().unwrap_or_else(|e| unreachable!("{e:#x}"));
    d.flush();
    let (j, left) = d.acquire_input().unwrap_or_else(|e| unreachable!("{e:#x}"));
    assert_ne!(i, j);
    assert_eq!(left, 2);
}

#[test]
fn bad_arguments_are_refused() {
    let (d, _io) = decoder();
    d.start();
    assert_eq!(d.send_input(-1, 0, 0, 0, 0), ERR_BAD_ARG);
    assert_eq!(d.send_input(0, 0, IN_CAP as i32 + 1, 0, 0), ERR_BAD_ARG);
    assert_eq!(d.send_input(IN_COUNT as i32, 0, 0, 0, 0), ERR_BAD_ARG);
}

#[test]
fn a_released_slot_lets_the_decoder_go_on() {
    let (d, _io) = decoder();
    d.start();
    let mut held = vec![];
    for n in 0..OUT_COUNT {
        send_key(&d, n as i64, 0);
        held.push(wait_output(&d).0);
    }
    send_key(&d, 99, 0);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(d.acquire_output(), Err(ERR_NO_OUTPUT));
    d.release_output(held[0] as usize);
    assert_eq!(wait_output(&d).1, 99);
}
