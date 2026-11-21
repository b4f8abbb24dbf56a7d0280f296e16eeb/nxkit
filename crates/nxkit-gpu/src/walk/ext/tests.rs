//! The tests of the extension records.

mod copy;

use super::copy::{buffer_copy_detail, buffer_fill_detail, clear_detail, copy_detail};
use super::draw::{draw_args, mesh_args};
use super::*;
use nxkit_metal::AddressMap;
use nxkit_nvn::mem::VecMem;

#[test]
fn indirect_and_mesh_draw_records_give_their_arguments() {
    let draw = |kind: DrawKind| Ext::new(ExtCode::Draw, kind as u16);
    let x = draw(DrawKind::MultiElementsIndirectCount)
        .u32(4)
        .u32(2)
        .u64(0x1000)
        .u64(0x2000)
        .u64(0x3000)
        .u32(9)
        .u64(20);
    assert_eq!(
        draw_args(&x),
        Some((
            4,
            DrawArgs::MultiIndirectCount {
                index: Some((2, 0x1000)),
                indirect: 0x2000,
                count: 0x3000,
                max: 9,
                stride: 20
            }
        ))
    );
    let x = draw(DrawKind::MultiArraysIndirectCount)
        .u32(5)
        .u64(0x2000)
        .u64(0x3000)
        .u32(3)
        .u64(16);
    assert_eq!(
        draw_args(&x),
        Some((
            5,
            DrawArgs::MultiIndirectCount {
                index: None,
                indirect: 0x2000,
                count: 0x3000,
                max: 3,
                stride: 16
            }
        ))
    );
    let x = draw(DrawKind::ElementsIndirect)
        .u32(4)
        .u32(1)
        .u64(0x1000)
        .u64(0x2000);
    assert_eq!(
        draw_args(&x),
        Some((
            4,
            DrawArgs::IndexedIndirect {
                index_type: 1,
                address: 0x1000,
                indirect: 0x2000
            }
        ))
    );
    let x = draw(DrawKind::ArraysIndirect).u32(0).u64(0x2000);
    assert_eq!(
        draw_args(&x),
        Some((0, DrawArgs::ArraysIndirect { indirect: 0x2000 }))
    );
    let x = draw(DrawKind::MeshTasks).u32(7).u32(2048);
    assert_eq!(
        mesh_args(&x),
        Some(DrawArgs::MeshTasks {
            first: 7,
            count: 2048
        })
    );
    assert_eq!(draw_args(&x), None);
    let x = draw(DrawKind::MeshTasksIndirect).u64(0x4000);
    assert_eq!(
        mesh_args(&x),
        Some(DrawArgs::MeshTasksIndirect { indirect: 0x4000 })
    );
    assert_eq!(
        mesh_args(&draw(DrawKind::Arrays).u32(4).u32(0).u32(3)),
        None
    );
}

#[test]
fn uniform_snapshot_keeps_the_unchanged_tail() {
    let map = AddressMap::new(0x1000, 0x2000, 128).unwrap();
    let mut w = Walker::new(Default::default(), map);
    let mut mem = VecMem::new(0x1000, 128);
    mem.write(0x1070, &[1, 2, 3, 4]).unwrap();
    w.updated.push((0x2000, 0x2010));
    let a = w.snapshot(&mem, 0x2000, 128).unwrap();
    assert_eq!(a.len(), 128);
    assert_eq!(&a[112..116], &[1, 2, 3, 4]);
    assert!(Arc::ptr_eq(&a, &w.snapshot(&mem, 0x2000, 128).unwrap()));

    // CPU writes between queue submissions must not reuse the previous snapshot.
    mem.write(0x1070, &[5, 6, 7, 8]).unwrap();
    w.submit(&mut mem, 1, &[]);
    let b = w.snapshot(&mem, 0x2000, 128).unwrap();
    assert_eq!(&b[112..116], &[5, 6, 7, 8]);
    assert_eq!(&a[112..116], &[1, 2, 3, 4]);
}

#[test]
fn a_range_with_the_same_bytes_again_in_the_frame_shares_its_buffer() {
    let map = AddressMap::new(0x1000, 0x2000, 128).unwrap();
    let mut w = Walker::new(Default::default(), map);
    w.copy_all = true;
    let mut mem = VecMem::new(0x1000, 128);
    mem.write(0x1000, &[9, 9]).unwrap();
    let a = w.snapshot(&mem, 0x2000, 128).unwrap();
    // The next submit asks again with the same bytes: the same buffer.
    w.snaps.get_mut().clear();
    let b = w.snapshot(&mem, 0x2000, 128).unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    // Other bytes give a new buffer, and the first one keeps its bytes.
    w.snaps.get_mut().clear();
    mem.write(0x1000, &[7, 7]).unwrap();
    let c = w.snapshot(&mem, 0x2000, 128).unwrap();
    assert!(!Arc::ptr_eq(&a, &c));
    assert_eq!((&a[..2], &c[..2]), (&[9u8, 9][..], &[7u8, 7][..]));
    // A new frame starts again.
    w.take();
    w.snaps.get_mut().clear();
    let d = w.snapshot(&mem, 0x2000, 128).unwrap();
    assert!(!Arc::ptr_eq(&c, &d));
}

#[test]
fn the_buffer_of_a_dropped_frame_comes_back_in_order() {
    let map = AddressMap::new(0x1000, 0x2000, 128).unwrap();
    let mut w = Walker::new(Default::default(), map);
    w.copy_all = true;
    let mem = VecMem::new(0x1000, 128);
    let first = w.snapshot(&mem, 0x2000, 64).unwrap();
    let second = w.snapshot(&mem, 0x2040, 64).unwrap();
    let (p1, p2) = (first.as_ptr(), second.as_ptr());
    w.take();
    w.snaps.get_mut().clear();
    // The frame is still held, so these are new buffers.
    let busy = w.snapshot(&mem, 0x2000, 64).unwrap();
    assert!(busy.as_ptr() != p1 && busy.as_ptr() != p2);
    drop((first, second, busy));
    w.snaps.get_mut().clear();
    w.take();
    let again = w.snapshot(&mem, 0x2000, 64).unwrap();
    assert_eq!(again.as_ptr(), p1);
    let again = w.snapshot(&mem, 0x2040, 64).unwrap();
    assert_eq!(again.as_ptr(), p2);
}

#[test]
fn an_inline_update_invalidates_a_cached_unmodified_range() {
    let map = AddressMap::new(0x1000, 0x2000, 128).unwrap();
    let mut walker = Walker::new(Default::default(), map);
    let mut mem = VecMem::new(0x1000, 128);
    assert!(walker.snapshot(&mem, 0x2000, 128).is_none());
    assert!(walker.snaps.borrow().contains_key(&(0x2000, 128)));
    let update = Ext::new(ExtCode::UpdateUniform, 4)
        .u32(0x2000)
        .u32(0)
        .u32(128)
        .u32(0)
        .u32(16)
        .u32(0)
        .u32(0x12345678);
    walker.update_uniform(&mut mem, &update);
    let snapshot = walker.snapshot(&mem, 0x2000, 128).unwrap();
    assert_eq!(&snapshot[16..20], &0x12345678u32.to_le_bytes());
}

#[test]
fn unbound_vertex_inputs_get_a_constant_stream() {
    let mut d = empty_draw();
    d.attributes.push(AttributeDetail {
        location: 0,
        format: 0x0a,
        offset: 0,
        stream: 0,
    });
    d.streams.push(StreamDetail {
        index: 0,
        address: 0x2000,
        size: 128,
        stride: 4,
        divisor: 0,
    });
    let inputs = [
        (0, nxkit_metal::VertexFormat::Float4),
        (7, nxkit_metal::VertexFormat::UInt4),
    ];
    let layout = crate::translate::vertex_descriptor(&d, &inputs).unwrap();
    assert_eq!(layout.attributes.len(), 2);
    assert_eq!(
        layout.attributes[0].1.buffer,
        crate::translate::stream_slot(0)
    );
    assert_eq!(layout.attributes[1].0, 7);
    assert_eq!(
        layout.attributes[1].1.format,
        nxkit_metal::VertexFormat::UInt4
    );
    assert_eq!(
        layout.attributes[1].1.buffer,
        crate::translate::DEFAULT_VERTEX_SLOT
    );
    assert!(
        layout
            .layouts
            .iter()
            .any(|l| l.buffer == crate::translate::DEFAULT_VERTEX_SLOT
                && l.step == nxkit_metal::StepFunction::Constant)
    );
    // The declared attribute itself also needs the fallback when its stream is unbound.
    d.streams.clear();
    let layout = crate::translate::vertex_descriptor(&d, &inputs).unwrap();
    assert!(
        layout
            .attributes
            .iter()
            .all(|(_, a)| a.buffer == crate::translate::DEFAULT_VERTEX_SLOT)
    );
}

#[test]
fn a_stream_with_stride_0_gets_a_constant_step_with_a_stride() {
    let mut d = empty_draw();
    d.attributes.push(AttributeDetail {
        location: 0,
        format: 0x0a,
        offset: 8,
        stream: 0,
    });
    d.streams.push(StreamDetail {
        index: 0,
        address: 0x2000,
        size: 128,
        stride: 0,
        divisor: 0,
    });
    let inputs = [(0, nxkit_metal::VertexFormat::Float4)];
    let layout = crate::translate::vertex_descriptor(&d, &inputs).unwrap();
    let l = layout
        .layouts
        .iter()
        .find(|l| l.buffer == crate::translate::stream_slot(0))
        .unwrap();
    // Metal refuses a stride of 0 even for a constant step.
    assert_eq!(l.step, nxkit_metal::StepFunction::Constant);
    assert_eq!(l.stride, 24);
}

fn copy_ext(code: ExtCode, arg: u16, words: &[u32]) -> Ext {
    words.iter().fold(Ext::new(code, arg), |x, w| x.u32(*w))
}

#[test]
fn a_dispatch_record_gives_direct_counts_or_the_indirect_address() {
    use super::records::dispatch_size;
    let direct = copy_ext(ExtCode::Dispatch, 0, &[4, 5, 6]);
    assert_eq!(dispatch_size(&direct), ([4, 5, 6], None));
    let indirect = copy_ext(ExtCode::Dispatch, 1, &[0x2000, 0x1]);
    assert_eq!(dispatch_size(&indirect), ([0; 3], Some(0x1_0000_2000)));
}

#[test]
fn an_event_signal_becomes_an_event_report_with_the_counter_word() {
    let map = AddressMap::new(0x1000, 0x2000, 0x100).unwrap();
    let mut w = Walker::new(Default::default(), map);
    let mut mem = VecMem::new(0x1000, 0x100);
    // The `NVNevent` at 0x1040 names its counter word at 0x10f0.
    mem.write(0x1040 + EVENT_COUNTER, &0x10f0u64.to_le_bytes())
        .unwrap();
    let mut st = State::default();
    let signal =
        |mode: u32, value: u32| copy_ext(ExtCode::SignalEvent, 0, &[0x1040, 0, mode, 0, 0, value]);
    w.ext(&mut mem, &mut st, &signal(0, 7));
    w.ext(
        &mut mem,
        &mut st,
        &signal(nxkit_nvn::marks::EVENT_SIGNAL_ADD, 2),
    );
    // An event without a counter (not initialised) writes nothing.
    w.ext(
        &mut mem,
        &mut st,
        &copy_ext(ExtCode::SignalEvent, 0, &[0x1080, 0, 0, 0, 0, 1]),
    );
    let reports = w.take().reports;
    assert_eq!(
        reports,
        vec![
            crate::detail::CounterReport {
                kind: crate::detail::REPORT_EVENT_WRITE,
                value: 7,
                addr: 0x10f0,
            },
            crate::detail::CounterReport {
                kind: crate::detail::REPORT_EVENT_ADD,
                value: 2,
                addr: 0x10f0,
            },
        ]
    );
    assert!(reports.iter().all(crate::detail::CounterReport::is_event));
}
