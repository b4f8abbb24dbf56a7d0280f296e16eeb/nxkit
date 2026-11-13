//! Origin and viewport-swizzle state must survive cached calls without leaking between queues.

use super::*;

fn subviewport() -> Record {
    ext(
        ExtCode::Viewport,
        0,
        &[11f32, 17.0, 59.0, 31.0].map(f32::to_bits),
    )
}

fn swizzle(y: u32) -> Record {
    ext(ExtCode::ViewportSwizzle, 0, &[0, y, 4, 6])
}

#[test]
fn device_and_queue_origins_select_distinct_draw_templates() {
    for_both(|p| {
        let child = p.put(&[draw()]);
        let top = p.put(&[
            program(PROG),
            subviewport(),
            ext(ExtCode::Scissor, 0, &[13, 19, 23, 7]),
            Record::Call(child),
        ]);
        for (device, queue) in [(true, true), (false, true), (false, false), (true, false)] {
            for rig in [&mut p.cached, &mut p.plain] {
                rig.w.device_origin(device);
                rig.w.queue_origin(1, queue);
            }
            for repeat in [false, true] {
                let (d, c) = p.frame(&[top]);
                let draw = &d.draws[0];
                let (y, height) = if device { (48.0, -31.0) } else { (17.0, 31.0) };
                assert_eq!(draw.dynamic.viewport, [11.0, y, 59.0, height]);
                assert_eq!(draw.dynamic.scissor, Some([13, 19, 23, 7]));
                assert_eq!(draw.dynamic.depth_range, [0.0, 1.0]);
                assert_eq!(draw.raster.front_face, u32::from(queue));
                assert_eq!(c.walk_hits, u32::from(repeat));
                assert_eq!(c.verify_mismatches, 0);
            }
        }
    });
}

#[test]
fn inherited_swizzle_selects_a_template_without_changing_winding() {
    for_both(|p| {
        let child = p.put(&[draw()]);
        let top = p.put(&[
            program(PROG),
            subviewport(),
            swizzle(2),
            Record::Call(child),
            swizzle(3),
            Record::Call(child),
            swizzle(2),
            Record::Call(child),
        ]);
        for expected_hits in [1, 3] {
            let (d, c) = p.frame(&[top]);
            assert_eq!(c.walk_hits, expected_hits);
            assert_eq!(c.verify_mismatches, 0);
            assert_eq!(d.draws[0].dynamic.viewport, [11.0, 48.0, 59.0, -31.0]);
            assert_eq!(d.draws[1].dynamic.viewport, [11.0, 17.0, 59.0, 31.0]);
            assert_eq!(d.draws[2].dynamic.viewport, d.draws[0].dynamic.viewport);
            assert!(d.draws.iter().all(|d| d.raster.front_face == 1));
        }
    });
}

#[test]
fn cached_swizzle_writes_survive_calls_and_submits_only_on_their_queue() {
    for_both(|p| {
        let child = p.put(&[swizzle(3)]);
        let top = p.put(&[
            program(PROG),
            subviewport(),
            swizzle(2),
            Record::Call(child),
            draw(),
        ]);
        let later = p.put(&[
            // Another viewport must not change viewport zero.
            ext(ExtCode::ViewportSwizzle, 1, &[0, 2, 4, 6]),
            draw(),
        ]);
        for expected_hits in [0, 1] {
            let (d, c) = p.frame(&[top, later]);
            assert_eq!(c.walk_hits, expected_hits);
            assert_eq!(c.verify_mismatches, 0);
            assert!(
                d.draws
                    .iter()
                    .all(|d| d.dynamic.viewport == [11.0, 17.0, 59.0, 31.0])
            );
        }
        let (d, _) = p.frame(&[later]);
        assert_eq!(d.draws[0].dynamic.viewport, [11.0, 17.0, 59.0, 31.0]);
        let other = p.put(&[program(PROG), subviewport(), draw()]);
        for rig in [&mut p.cached, &mut p.plain] {
            rig.w.submit(&mut rig.mem, 2, &[CommandHandle(other)]);
            let d = rig.w.take();
            assert_eq!(d.draws[0].dynamic.viewport, [11.0, 48.0, 59.0, -31.0]);
            assert_eq!(d.draws[0].raster.front_face, 1);
        }
    });
}

#[test]
fn a_present_captures_the_device_origin_not_the_queue_origin() {
    let mut rig = Rig::new();
    rig.w.device_origin(true);
    rig.w.queue_origin(1, false);
    let first = rig.w.current_origin();
    rig.w.device_origin(false);
    rig.w.queue_origin(1, true);
    let second = rig.w.current_origin();
    assert!(first);
    assert!(!second);
}
