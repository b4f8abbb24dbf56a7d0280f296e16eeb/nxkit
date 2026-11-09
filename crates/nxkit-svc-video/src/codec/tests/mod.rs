//! Tests of the buffer queues, the sync handles and the native images.

use super::*;

mod worker;

fn dec() -> Dec {
    Dec::new(1, Codec::Vp9, 0)
}

#[test]
fn nn_results_carry_the_description() {
    assert_eq!(BUSY, 0x6f | (3603 << 9));
    assert_eq!(NONE, 0x6f | (3604 << 9));
}

#[test]
fn registered_buffers_are_acquired_in_order() {
    let mut d = dec();
    assert_eq!(d.register(2, None, 0), 0);
    assert_eq!(d.register(5, None, 1), 0);
    assert_eq!(d.acquire().map(|(i, _)| i), Ok(2));
    assert_eq!(d.acquire().map(|(i, _)| i), Ok(5));
    assert_eq!(d.acquire().map(|(i, _)| i), Err(NONE));
}

#[test]
fn a_buffer_that_does_not_exist_is_refused() {
    let mut d = dec();
    assert_eq!(d.register(BUFS as u32, None, 0), BAD);
    assert_eq!(d.unregister(BUFS as u32), BAD);
}

#[test]
fn the_acquire_queue_is_eight_deep() {
    let mut d = dec();
    for i in 0..10 {
        d.register(i, None, 0);
    }
    assert_eq!(d.fifo.len(), FIFO);
}

#[test]
fn a_picture_goes_into_the_first_released_buffer() {
    let mut d = dec();
    d.register(0, None, 0);
    let (i, sync) = d.acquire().unwrap_or_else(|_| unreachable!());
    let mut fx = Effects::default();
    d.release(i, &mut fx);
    assert!(!d.reached(&sync) || sync.generation == 0);
    d.held.push_back(Held {
        frame: Arc::new(grey()),
        ts: 7,
        eos: false,
    });
    d.drain(true, &mut fx);
    assert_eq!(d.properties(0), Ok((2, 2, 7, false)));
    assert_eq!(fx.filled.len(), 1);
}

#[test]
fn a_handle_of_the_old_release_is_not_reached_before_the_picture() {
    let mut d = dec();
    d.register(0, None, 0);
    let mut fx = Effects::default();
    d.release(0, &mut fx);
    let (_, sync) = d.acquire().unwrap_or_else(|_| unreachable!());
    assert!(!d.reached(&sync));
    d.held.push_back(Held {
        frame: Arc::new(grey()),
        ts: 0,
        eos: false,
    });
    d.drain(true, &mut fx);
    assert!(d.reached(&sync));
}

#[test]
fn the_end_of_stream_gets_a_buffer_of_its_own() {
    let mut d = dec();
    d.register(3, None, 0);
    let mut fx = Effects::default();
    d.release(3, &mut fx);
    d.eos_pending = true;
    d.drain(true, &mut fx);
    assert_eq!(d.properties(3), Ok((0, 0, 0, true)));
    assert!(!d.eos_pending);
}

#[test]
fn a_full_queue_refuses_packets_and_clears_the_event() {
    let mut d = Dec::new(1, Codec::Vp9, 0x1000);
    for _ in 0..HOLD {
        d.held.push_back(Held {
            frame: Arc::new(grey()),
            ts: 0,
            eos: false,
        });
    }
    let mut fx = Effects::default();
    assert_eq!(d.submit(&[1], 0, false, &mut fx), Err(BUSY));
    assert_eq!(fx.event, Some(false));
}

#[test]
fn the_event_goes_down_at_ready_pictures_and_up_when_room_comes_back() {
    let mut d = Dec::new(1, Codec::Vp9, 0x1000);
    for _ in 0..READY {
        d.held.push_back(Held {
            frame: Arc::new(grey()),
            ts: 0,
            eos: false,
        });
    }
    let mut fx = Effects::default();
    d.drain(false, &mut fx);
    assert_eq!(fx.event, Some(false));
    d.register(0, None, 0);
    d.release(0, &mut fx);
    assert_eq!(d.held.len(), READY - 1);
    assert_eq!(fx.event, Some(true));
}

#[test]
fn unregister_drops_the_buffer_from_both_queues() {
    let mut d = dec();
    d.register(1, None, 0);
    let mut fx = Effects::default();
    d.release(1, &mut fx);
    assert_eq!(d.unregister(1), 0);
    assert!(d.fifo.is_empty() && d.pend.is_empty());
}

#[test]
fn a_foreign_sync_handle_reads_as_signalled() {
    assert!(sync_reached(&SyncHandle::default()));
    assert!(sync_reached(&SyncHandle {
        magic: SYNC_MAGIC,
        dec: 1,
        idx: 0,
        generation: 0
    }));
}

#[test]
fn native_sizes_follow_the_page_and_the_row_alignment() {
    assert_eq!(native_stride(1920), 7680);
    assert_eq!(native_stride(100), 448);
    assert_eq!(native_size(1920, 1080) % NATIVE_ALIGN, 0);
    assert!(native_size(1920, 1080) >= 7680 * 1080);
}

#[test]
fn a_blank_image_has_its_memory_until_it_is_freed() {
    let h = native_image_blank(16, 16);
    assert_ne!(h, 0);
    let img = native_image(h);
    assert!(img.is_some_and(|i| i.width == 16
        && i.stride == blank_stride(16)
        && i.mem % NATIVE_ALIGN == 0));
    native_image_blank_free(h);
    assert_eq!(native_image(h), None);
}

fn grey() -> Frame {
    Frame::from_nv12(
        2,
        2,
        3,
        crate::frame::Nv12 {
            y: vec![16; 4],
            y_stride: 2,
            uv: vec![128; 2],
            uv_stride: 2,
        },
    )
}
