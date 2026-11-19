//! Patch settings survive nested recording replay and stay local to the submitting queue.

use super::*;

#[test]
fn replay_preserves_explicit_patch_settings_and_unwritten_fields() {
    for_both(|p| {
        let child = p.put(&[
            ext(ExtCode::Setting, Setting::PatchSize as u16, &[3]),
            ext(
                ExtCode::Setting,
                Setting::InnerTessellationLevels as u16,
                &[2.5f32.to_bits(), 0x7fc0_0011],
            ),
        ]);
        for (i, level) in [1.0f32, 2.0].into_iter().enumerate() {
            let outer = [level.to_bits(); 4];
            let top = p.put(&[
                ext(
                    ExtCode::Setting,
                    Setting::OuterTessellationLevels as u16,
                    &outer,
                ),
                Record::Call(child),
            ]);
            let (_, counters) = p.frame(&[top]);
            assert_eq!(counters.walk_hits, i as u32);
            assert_eq!(counters.verify_mismatches, 0);
            for rig in [&p.cached, &p.plain] {
                let patch = rig.w.queues[&1].patch;
                assert_eq!(patch.size, Some(3));
                assert_eq!(patch.inner, Some([2.5f32.to_bits(), 0x7fc0_0011]));
                assert_eq!(patch.outer, Some(outer));
            }
        }
        let other = p.put(&[program(PROG), draw()]);
        for rig in [&mut p.cached, &mut p.plain] {
            rig.w.submit(&mut rig.mem, 2, &[CommandHandle(other)]);
            assert_eq!(rig.w.queues[&2].patch, Default::default());
        }
    });
}

#[test]
fn patch_draw_tracks_bound_evaluation_resources_and_inherited_size() {
    let dir = std::env::temp_dir().join(format!("nxkit-patch-stages-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("0101010101010101_b.json"), r#"{"stage":"vs"}"#).unwrap();
    std::fs::write(dir.join("0202020202020202_b.json"), r#"{"stage":"ps"}"#).unwrap();
    std::fs::write(dir.join("0303030303030303_b.json"), r#"{"stage":"ds"}"#).unwrap();
    for_both(|p| {
        for rig in [&mut p.cached, &mut p.plain] {
            rig.w.programs = crate::walk::Programs::new(dir.clone());
            let mut keys = keys(1, 2).to_vec();
            keys.push(StageKey {
                // The index in SetShaders is an array index, not the shader kind.
                index: 2,
                key: Some(ShaderKey([3; 8])),
                code: 0,
                code_len: 0,
            });
            rig.w.program_set(PROG, &keys);
        }
        let [lo, hi] = halves(PROG);
        let child = p.put(&[ext(
            ExtCode::Draw,
            DrawKind::Arrays as u16,
            &[crate::detail::PATCHES, 0, 12],
        )]);
        let top = p.put(&[
            ext(ExtCode::Program, 0, &[lo, hi, 0b1_1011]),
            ubo(4 << 8, UBO_AT, 64),
            ext(ExtCode::Setting, Setting::PatchSize as u16, &[3]),
            Record::Call(child),
            ext(ExtCode::Setting, Setting::PatchSize as u16, &[4]),
            Record::Call(child),
            ext(ExtCode::Setting, Setting::PatchSize as u16, &[3]),
            Record::Call(child),
        ]);
        let (detail, counters) = p.frame(&[top]);
        assert_eq!(counters.walk_hits, 1);
        assert_eq!(counters.verify_mismatches, 0);
        let draws = &detail.draws;
        assert_eq!(draws.len(), 3);
        for (draw, size) in draws.iter().zip([3, 4, 3]) {
            let patch = draw.patch.as_ref().unwrap();
            assert_eq!(patch.size, Some(size));
            assert!(patch.reflected);
            assert_eq!(patch.control, None);
            assert_eq!(patch.evaluation.as_deref(), Some("0303030303030303"));
            assert!(draw.ubos.iter().any(|u| {
                u.stage == crate::detail::Stage::TessEvaluation && u.address == UBO_AT
            }));
        }
        let geometry = PROG + 0x100;
        let [g0, g1] = halves(geometry);
        let unsupported = p.put(&[
            ext(ExtCode::Program, 0, &[g0, g1, 0b100]),
            Record::Call(child),
            ext(ExtCode::Program, 0, &[0, 0, 0b100]),
            Record::Call(child),
        ]);
        let (detail, counters) = p.frame(&[unsupported]);
        assert_eq!(counters.verify_mismatches, 0);
        assert_eq!(detail.draws.len(), 2);
        assert!(!detail.draws[0].patch.as_ref().unwrap().reflected);
        assert!(detail.draws[1].patch.as_ref().unwrap().reflected);
    });
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn indexed_patch_tracks_inherited_primitive_restart() {
    let dir = std::env::temp_dir().join(format!("nxkit-indexed-patch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("0101010101010101_b.json"), r#"{"stage":"vs"}"#).unwrap();
    for_both(|p| {
        for rig in [&mut p.cached, &mut p.plain] {
            rig.w.programs = crate::walk::Programs::new(dir.clone());
            rig.w.program_set(PROG, &keys(1, 2)[..1]);
        }
        let [lo, hi] = halves(GPU + 0x100);
        let child = p.put(&[ext(
            ExtCode::Draw,
            DrawKind::Elements as u16,
            &[crate::detail::PATCHES, 2, 6, lo, hi, (-2i32) as u32, 0, 1],
        )]);
        let top = p.put(&[
            program(PROG),
            ext(ExtCode::Setting, Setting::PatchSize as u16, &[3]),
            ext(
                ExtCode::Setting,
                Setting::PrimitiveRestart as u16,
                &[1, u32::MAX],
            ),
            Record::Call(child),
            ext(
                ExtCode::Setting,
                Setting::PrimitiveRestart as u16,
                &[0, u32::MAX],
            ),
            Record::Call(child),
            ext(
                ExtCode::Setting,
                Setting::PrimitiveRestart as u16,
                &[1, u32::MAX],
            ),
            Record::Call(child),
        ]);
        let (frame, counters) = p.frame(&[top]);
        assert_eq!(counters.walk_hits, 1);
        assert_eq!(counters.verify_mismatches, 0);
        for (draw, restart) in frame.draws.iter().zip([true, false, true]) {
            assert_eq!(draw.patch.as_ref().unwrap().restart, restart);
            assert!(matches!(
                draw.args,
                crate::detail::DrawArgs::Indexed {
                    index_type: 2,
                    count: 6,
                    base_vertex: -2,
                    ..
                }
            ));
        }
    });
    std::fs::remove_dir_all(dir).unwrap();
}
