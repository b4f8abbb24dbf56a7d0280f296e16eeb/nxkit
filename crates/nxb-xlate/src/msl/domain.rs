//! Lowering for a triangular, fractional-odd, counterclockwise domain shader.

use super::emit::{Em, R};
use super::entry::Param;
use super::{Context, Failure, Translated, failure, library, options, structure_job};
use crate::ir::{Inst, IoVariable, StorageKind};
use crate::job::Job;
use crate::structured::ast::OpData;
use crate::structured::types::AggType;

/// Full logical capture (144 words per vertex), at render-stage buffer 26.
pub const CAPTURE_SLOT: u32 = 26;
/// Expanded domain records, at render-stage buffer 27.
pub const POINT_SLOT: u32 = 27;
/// Draw-time patch size and levels, at render-stage buffer 28.
pub const STATE_SLOT: u32 = 28;
/// Per-ordinal validation results from the opt-in indexed capture, at buffer 29.
pub const VALIDITY_SLOT: u32 = 29;
/// Validating evaluation entry for indexed draws requiring depth conversion.
pub const INDEXED_ENTRY: &str = "main0_tess_indexed_domain";
/// Validating evaluation entry for indexed draws already using Metal clip depth.
pub const INDEXED_ENTRY_Z: &str = "main0_tess_indexed_domain_z";

/// The supported mode, established by inspecting an explicit control section.
pub struct TriangleFractionalOddCcw(());

impl TriangleFractionalOddCcw {
    /// Validate the little-endian primitive, spacing, and winding enums.
    /// No defaults are applied to absent, truncated, or unsupported metadata.
    pub fn from_control(control: &[u8]) -> Result<Self, Failure> {
        let read = |at| {
            control
                .get(at..at + 4)
                .and_then(|b| <[u8; 4]>::try_from(b).ok().map(u32::from_le_bytes))
        };
        match (read(0x738), read(0x73c), read(0x744)) {
            (Some(primitive), Some(spacing), Some(winding)) => {
                Self::from_words([primitive, spacing, winding])
            }
            _ => Err(failure("unported: missing domain control metadata".into())),
        }
    }

    fn from_words(words: [u32; 3]) -> Result<Self, Failure> {
        if words == [0x0004, 0x8e7b, 0x0901] {
            Ok(Self(()))
        } else {
            Err(failure("unported: domain mode".into()))
        }
    }
}

pub(super) fn translate_native(
    job: &Job,
    code: &[u8],
    ctx: &Context<'_>,
) -> Result<Translated, Failure> {
    if job.arch_sm86 {
        return Err(failure("unported: NVN2 domain shader".into()));
    }
    let words = job
        .nvn_domain_mode
        .ok_or_else(|| failure("unported: missing domain control metadata".into()))?;
    let mode = TriangleFractionalOddCcw::from_words(words)?;
    translate(job, code, ctx, &mode, false)
}

/// Translate only for an explicit diagnostic capture/expansion harness. This does not
/// enable replay of producer-generated domain shaders or claim support for their draw-time resources.
///
/// The caller must pair this with the no-hull draw path and supply validated capture and
/// expansion buffers and the stage's own resource table. The two vertex entries differ
/// only in depth conversion: `main0` converts clip Z, `main0_z` leaves it unchanged.
pub fn translate_diagnostic(
    job: &Job,
    code: &[u8],
    ctx: &Context<'_>,
    mode: &TriangleFractionalOddCcw,
) -> Result<Translated, Failure> {
    translate(job, code, ctx, mode, true)
}

fn translate(
    job: &Job,
    code: &[u8],
    ctx: &Context<'_>,
    _mode: &TriangleFractionalOddCcw,
    diagnostic: bool,
) -> Result<Translated, Failure> {
    if job.stage != "ds" {
        return Err(failure("domain translation needs a ds job".into()));
    }
    let structured = structure_job(job, code).map_err(failure)?;
    if structured.props.definitions.stage != Some(crate::attr::Stage::TessellationEvaluation) {
        return Err(failure(
            "domain translation needs a domain shader header".into(),
        ));
    }
    library::build_domain(&structured, ctx, &options("ds", code, ctx), diagnostic).map_err(failure)
}

/// V1 attests to the actual entries: full logical VS capture, or a declared float4
/// user interface. Missing or unsupported declarations have no attestation.
pub(super) fn linkage(
    mode: library::Mode,
    shader: super::emit::Shader,
    plan: &super::io::Plan,
) -> Option<nxb_pyjson::Value> {
    use super::emit::Shader;
    use nxb_pyjson::Value;

    let mask = |locs: &mut dyn Iterator<Item = u32>| {
        let mut mask = 0;
        for loc in locs {
            mask |= 1u32.checked_shl(loc)?;
        }
        Some(mask)
    };
    let (role, locs) = match (mode, shader) {
        (library::Mode::Production, Shader::Vertex) => ("capture144", None),
        (library::Mode::Domain { diagnostic: false }, Shader::Vertex)
            if plan.out_members.len() == plan.user_locs.len() + 1 =>
        {
            ("domain", Some(mask(&mut plan.user_locs.iter().copied())?))
        }
        (library::Mode::Production, Shader::Fragment)
            if plan
                .varyings
                .iter()
                .all(|v| v.msl_type == "float4" && v.interpolation.is_empty())
                && plan
                    .in_members
                    .iter()
                    .filter(|m| !m.contains("[[position]]"))
                    .count()
                    == plan.varyings.len() =>
        {
            (
                "fragment",
                Some(mask(&mut plan.varyings.iter().map(|v| v.location))?),
            )
        }
        _ => return None,
    };
    let mut v = Value::obj([("version", Value::Int(1)), ("role", Value::str(role))]);
    if let Some(locs) = locs {
        v.set("float4_locs", Value::Int(i64::from(locs)));
    }
    Some(v)
}

pub(super) fn params() -> [Param; 4] {
    [
        Param::new(
            "device const uint* dm_capture",
            format!("buffer({CAPTURE_SLOT})"),
            "dm_capture",
        ),
        Param::new(
            "device const dm_domain_vertex* dm_points",
            format!("buffer({POINT_SLOT})"),
            "dm_points",
        ),
        Param::new(
            "constant dm_domain_state& dm_domain",
            format!("buffer({STATE_SLOT})"),
            "dm_domain",
        ),
        Param::new("uint dm_vertex", "vertex_id", "dm_vertex"),
    ]
}

pub(super) fn indexed_validity() -> Param {
    Param::new(
        "device const uint* dm_valid",
        format!("buffer({VALIDITY_SLOT})"),
        "dm_valid",
    )
}

pub(super) fn indexed_guard() -> [String; 5] {
    [
        "main0_out dm_clipped = {};".into(),
        "dm_clipped.gl_Position = float4(2.0, 2.0, 2.0, 1.0);".into(),
        "uint dm_start = dm_points[dm_vertex].ids.x, dm_count = dm_domain.info.x;".into(),
        "if (dm_domain.info.y != 1u || dm_count == 0u || dm_count > 32u || dm_start > dm_domain.info.z || dm_count > dm_domain.info.z - dm_start) return dm_clipped;".into(),
        "for (uint dm_cp = 0u; dm_cp < dm_count; dm_cp += 1u) { if (dm_valid[dm_start + dm_cp] != 1u) return dm_clipped; }".into(),
    ]
}

// IDs are (first captured vertex ordinal, original patch ID, reserved, reserved).
// State is (uint4 patch info, float4 outer factors, float4 inner factors). info.x
// is the input control point count. For indexed replay, info.y is 1 and info.z
// is the number of available capture records. The host validates buffer bindings.
pub(super) fn declarations() -> String {
    let words = super::variant::vertex::CAPTURE_WORDS;
    format!(
        "struct dm_domain_vertex {{ float4 coord; uint4 ids; }};\n\
         struct dm_domain_state {{ uint4 info; float4 outer; float4 inner; }};\n\
         static float dm_domain_cp(device const uint* data, uint base, uint count, uint cp, uint word)\n\
         {{\n    if (cp >= count) return 0.0;\n    return as_type<float>(data[(base + cp) * {words}u + word]);\n}}\n\
         static float dm_domain_user(device const uint* data, uint base, uint count, uint cp, uint loc, uint c)\n\
         {{\n    if (loc >= 32u || c >= 4u) return 0.0;\n    return dm_domain_cp(data, base, count, cp, 16u + loc * 4u + c);\n}}\n"
    )
}

/// The domain runtime must never turn unknown, raw descriptor names into implicit guest slot zero.
pub(super) fn check_bindings(bindings: &[super::refl::Binding]) -> R<()> {
    use super::abi::Kind;
    for b in bindings {
        if matches!(
            b.kind,
            Kind::Cbuf
                | Kind::Sbuf
                | Kind::UboPtr
                | Kind::Tex
                | Kind::Tbuf
                | Kind::Img
                | Kind::Samp
                | Kind::SampTex
        ) && b
            .reg
            .is_none_or(|slot| slot < 0 || slot > i64::from(u32::MAX))
        {
            return Err(format!(
                "unported: unresolved domain {} binding {}",
                b.kind.as_str(),
                b.msl_name
            ));
        }
    }
    Ok(())
}

pub(super) fn check_input(storage: StorageKind, variable: IoVariable) -> R<()> {
    if matches!(
        (storage, variable),
        (
            StorageKind::Input,
            IoVariable::Position
                | IoVariable::UserDefined
                | IoVariable::TessellationCoord
                | IoVariable::PrimitiveId
                | IoVariable::PatchVertices
        ) | (
            StorageKind::InputPerPatch,
            IoVariable::TessellationLevelInner | IoVariable::TessellationLevelOuter
        )
    ) {
        return Ok(());
    }
    Err(format!(
        "unported: domain input {} in storage {}",
        variable.0, storage.0
    ))
}

impl Em<'_> {
    pub(super) fn domain_input(&mut self, inst: Inst, o: &OpData) -> R<super::inst_mem::Place> {
        if inst != Inst::Load {
            return Err("unported: write to domain input".into());
        }
        let var = IoVariable(
            self.constant(self.src(o, 0)?)
                .ok_or("domain input without constant variable")? as u32,
        );
        check_input(o.storage_kind, var)?;
        let shape = |n| {
            (o.sources.len() == n)
                .then_some(())
                .ok_or("unported: domain input operand shape")
        };
        let component = |em: &Self, i, n| -> R<i32> {
            em.constant(em.src(o, i)?)
                .filter(|c| (0..n).contains(c))
                .ok_or("unported: nonconstant or out of range domain input component".into())
        };
        let (s, ty) = match var {
            IoVariable::PrimitiveId | IoVariable::PatchVertices => {
                shape(1)?;
                let s = if var == IoVariable::PrimitiveId {
                    "dm_points[dm_vertex].ids.y"
                } else {
                    "dm_domain.info.x"
                };
                (s.into(), AggType::U32)
            }
            IoVariable::TessellationCoord
            | IoVariable::TessellationLevelInner
            | IoVariable::TessellationLevelOuter => {
                shape(2)?;
                let (base, n) = match var {
                    IoVariable::TessellationCoord => ("dm_points[dm_vertex].coord", 2),
                    IoVariable::TessellationLevelInner => ("dm_domain.inner", 2),
                    _ => ("dm_domain.outer", 4),
                };
                let c: i32 = component(self, 1, n)?;
                (format!("{base}[{c}]"), AggType::FP32)
            }
            IoVariable::Position | IoVariable::UserDefined => {
                let user = var == IoVariable::UserDefined;
                shape(if user { 4 } else { 3 })?;
                // The attrmap emitter orders a fixed varying's location before its
                // control point. An indexed varying instead uses the array form
                // (control point, location, component). Position has no location.
                let indexed = user && self.props.definitions.ia_indexing;
                let cp_at = if user && !indexed { 2 } else { 1 };
                let cp = self.get(AggType::U32, self.src(o, cp_at)?)?.s;
                let args =
                    format!("dm_capture, dm_points[dm_vertex].ids.x, dm_domain.info.x, {cp}");
                let read = if indexed {
                    let loc = self.get(AggType::U32, self.src(o, 2)?)?.s;
                    let c = self.get(AggType::U32, self.src(o, 3)?)?.s;
                    format!("dm_domain_user({args}, {loc}, {c})")
                } else {
                    let c = component(self, if user { 3 } else { 2 }, 4)?;
                    let word = if user {
                        let l = component(self, 1, 32)?;
                        16 + 4 * l + c
                    } else {
                        c
                    };
                    format!("dm_domain_cp({args}, {word}u)")
                };
                (read, AggType::FP32)
            }
            _ => return Err("unported: domain input".into()),
        };
        Ok(super::inst_mem::Place {
            s,
            ty,
            mem: ty,
            space: super::inst_mem::Space::Thread,
            skip: false,
            mask: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::parse_jobs;
    use crate::translation::emit_test_util::PRED7;

    fn control() -> Vec<u8> {
        let mut bytes = vec![0; 0x748];
        for (offset, word) in [(0x738, 4u32), (0x73c, 0x8e7b), (0x744, 0x901)] {
            bytes[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    fn ald(reg: u64, offset: u64, patch: bool) -> u64 {
        (0b11101111110110u64 << 50)
            | reg
            | (255 << 8)
            | PRED7
            | (offset << 20)
            | (u64::from(patch) << 31)
            | (255 << 39)
    }

    fn s2r(reg: u64, sreg: u64) -> u64 {
        (0b11110000110010u64 << 50) | reg | PRED7 | (sreg << 20)
    }

    fn ldc(reg: u64, bank: u64, byte_offset: u64) -> u64 {
        let b32 = nxb_maxwell::enums::LsSize2::B32.0 as u64;
        (0b11101111100100u64 << 50)
            | reg
            | (255 << 8)
            | PRED7
            | (byte_offset << 20)
            | (bank << 36)
            | (b32 << 48)
    }

    fn ast(reg: u64, offset: u64) -> u64 {
        (0b11101111111100u64 << 50) | reg | (255 << 8) | PRED7 | (offset << 20) | (255 << 39)
    }

    #[test]
    fn rejects_truncated_and_other_modes() {
        let mut bytes = control();
        assert!(TriangleFractionalOddCcw::from_control(&bytes).is_ok());
        assert!(TriangleFractionalOddCcw::from_control(&bytes[..0x747]).is_err());
        bytes[0x744] = 0;
        assert!(TriangleFractionalOddCcw::from_control(&bytes).is_err());
    }

    #[test]
    fn domain_fixture() {
        let job = parse_jobs(r#"[{"key":"synthetic","stage":"ds"}]"#)
            .unwrap()
            .remove(0);
        let ops = [
            ald(0, 0x2f0, false),
            ald(1, 0x000, true),
            ald(2, 0x0b4, false), // varying location 3, control point 0, component y
            ald(3, 0x060, false), // original patch ID
            ald(4, 0x014, true),  // second inner level
            s2r(5, 29),           // invocation information, including patch size in the high half
            ald(6, 0x078, false), // captured position.z, not a generic varying
            ldc(7, 5, 0x30),      // evaluation-stage UBO slot 2
            ast(0, 0x070),
            ast(1, 0x074),
            ast(2, 0x078),
            ast(3, 0x0a0),
            ast(4, 0x0a4),
            ast(5, 0x0a8),
            ast(6, 0x0ac),
            ast(7, 0x0b0),
        ];
        let mut code = super::super::geom_tests::code(0, &ops);
        code[..4].copy_from_slice(&(3u32 << 10).to_le_bytes());
        let mode = TriangleFractionalOddCcw::from_control(&control()).unwrap();
        let linked = translate_diagnostic(&job, &code, &Context::default(), &mode).unwrap();
        let library = &linked.libraries[0];
        let msl = &library.msl;
        for fragment in [
            "vertex main0_out main0(",
            "dm_capture [[buffer(26)]]",
            "dm_points [[buffer(27)]]",
            "dm_domain [[buffer(28)]]",
            "dm_points[dm_vertex].coord[0]",
            "dm_domain.outer[0]",
            "dm_domain.info.x, 0u, 29u)",
            "dm_points[dm_vertex].ids.y",
            "dm_domain.inner[1]",
            "as_type<float>(dm_domain.info.x)",
            " << 16)",
            "spvDescriptorSet0 [[buffer(0)]]",
            "spvDescriptorSet0.tep_c5",
            "dm_domain.info.x, 0u, 2u)",
            "loc2 [[user(locn2)]]",
        ] {
            assert!(msl.contains(fragment), "missing {fragment}\n{msl}");
        }
        assert!(!msl.contains("[[stage_in]]"), "{msl}");
        let (converted, raw) = msl.split_once("vertex main0_out main0_z(").unwrap();
        assert!(converted.contains("fma(out.gl_Position.z"), "{msl}");
        let (raw_direct, indexed) = raw
            .split_once("vertex main0_out main0_tess_indexed_domain(")
            .unwrap();
        assert!(!raw_direct.contains("fma(out.gl_Position.z"), "{msl}");
        let (checked, checked_raw) = indexed
            .split_once("vertex main0_out main0_tess_indexed_domain_z(")
            .unwrap();
        for e in [checked, checked_raw] {
            assert!(e.contains("dm_valid [[buffer(29)]]"), "{e}");
            let validate = e.find("dm_valid[dm_start + dm_cp]").unwrap();
            let guest = e.find("(*spvDescriptorSet0.tep_c5)").unwrap();
            assert!(validate < guest, "{e}");
        }
        assert!(checked.contains("fma(out.gl_Position.z"), "{checked}");
        assert!(
            !checked_raw.contains("fma(out.gl_Position.z"),
            "{checked_raw}"
        );
        let reflection = library.reflection.as_ref().unwrap();
        assert_eq!(reflection.stage, "ds");
        assert!(reflection.vertex_inputs.is_empty());
        assert!(reflection.bindings.iter().any(|b| b.msl_name == "tep_c5"
            && b.kind == super::super::abi::Kind::Cbuf
            && b.reg == Some(2)));
        let abi = super::super::geom_tests::extra(reflection, "domain_replay").unwrap();
        assert_eq!(
            abi.get("version").and_then(nxb_pyjson::Value::as_i64),
            Some(1)
        );
        assert_eq!(
            abi.get("capture_words").and_then(nxb_pyjson::Value::as_i64),
            Some(144)
        );
        assert!(reflection.entries.iter().any(|(k, _)| k == "entry_z"));
        assert_eq!(
            abi.get("indexed_capture_version")
                .and_then(nxb_pyjson::Value::as_i64),
            Some(1)
        );
        assert_eq!(linked.libraries.len(), 1);
        assert!(super::super::translate(&job, &code, &Context::default()).is_err());
        let mut mislabeled = job.clone();
        mislabeled.stage = "vs".into();
        assert!(super::super::translate(&mislabeled, &code, &Context::default()).is_err());
    }

    #[test]
    fn indexed_varying_keeps_dynamic_location_and_control_point_separate() {
        let job = parse_jobs(r#"[{"key":"synthetic","stage":"ds"}]"#)
            .unwrap()
            .remove(0);
        // ALD.PHYS takes the attribute byte offset from R1 and the control point
        // from R2. Make them independent run-time values, not compile-time zeros.
        let physical = (ald(3, 0, false) & !((255 << 8) | (255 << 39))) | (1 << 8) | (2 << 39);
        let mut code = super::super::geom_tests::code(
            0,
            &[
                ald(1, 0x060, false), // patch ID as varying byte offset
                s2r(2, 29),           // patch size-derived control point
                physical,
                ast(3, 0x070),
            ],
        );
        code[..4].copy_from_slice(&(3u32 << 10).to_le_bytes());
        let mode = TriangleFractionalOddCcw::from_control(&control()).unwrap();
        let translated = translate_diagnostic(&job, &code, &Context::default(), &mode).unwrap();
        let msl = &translated.libraries[0].msl;
        assert!(msl.contains("if (loc >= 32u || c >= 4u)"), "{msl}");
        assert!(
            msl.contains("dm_domain_user(dm_capture, dm_points[dm_vertex].ids.x, dm_domain.info.x"),
            "{msl}"
        );
        assert!(msl.contains("dm_domain.info.x"), "{msl}");
        assert!(msl.contains("dm_points[dm_vertex].ids.y"), "{msl}");
        let lhs = |marker: &str| {
            msl.lines()
                .find(|line| line.contains(marker))
                .and_then(|line| line.split_once(" = "))
                .map(|(lhs, _)| lhs.trim())
                .unwrap()
        };
        let cp = lhs(" << 16)");
        let loc = lhs(" >> 4u");
        let component = lhs(" & 3)");
        let read = format!(
            "dm_domain_user(dm_capture, dm_points[dm_vertex].ids.x, dm_domain.info.x, as_type<uint>({cp}), {loc}, as_type<uint>({component}))"
        );
        assert!(msl.contains(&read), "{msl}");
    }

    #[test]
    fn a_domain_storage_binding_does_not_implicitly_use_slot_zero() {
        use super::super::{abi::Kind, refl::Binding};
        let mut b = Binding::new("tep_storage", "const device uint*", 0, Kind::Sbuf);
        assert!(check_bindings(std::slice::from_ref(&b)).is_err());
        b.reg = Some(-1);
        assert!(check_bindings(std::slice::from_ref(&b)).is_err());
        b.reg = Some(0);
        assert!(check_bindings(std::slice::from_ref(&b)).is_ok());
        let c1 = Binding::new("tep_c1", "constant uint*", 1, Kind::C1);
        assert!(check_bindings(&[c1]).is_ok());
    }

    #[test]
    fn refuses_custom_patch_values_without_a_hull() {
        assert!(check_input(StorageKind::InputPerPatch, IoVariable::UserDefined).is_err());
        assert!(
            check_input(
                StorageKind::InputPerPatch,
                IoVariable::TessellationLevelOuter
            )
            .is_ok()
        );
    }
}
