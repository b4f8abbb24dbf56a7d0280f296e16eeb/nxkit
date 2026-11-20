//! Lowering for a triangular, fractional-odd, counterclockwise domain shader.

use super::emit::{Em, R};
use super::entry::Param;
use super::{Context, Failure, Translated, failure, library, options, structure_job};
use crate::ir::{Inst, IoVariable, StorageKind};
use crate::job::Job;
use crate::structured::ast::OpData;
use crate::structured::types::AggType;

/// Full logical capture (144 words per vertex), at render-stage buffer 26.
const CAPTURE_SLOT: u32 = 26;
/// Expanded domain records, at render-stage buffer 27.
const POINT_SLOT: u32 = 27;
/// Draw-time patch size and levels, at render-stage buffer 28.
const STATE_SLOT: u32 = 28;
// Encoded NVN1 domain control words, in primitive / spacing / winding order.
const TESS_PRIMITIVE_TRIANGLES: u32 = 4;
const TESS_SPACING_FRACTIONAL_ODD: u32 = 0x8e7b;
const TESS_WINDING_CCW: u32 = 0x0901;
const TRIANGLE_FRACTIONAL_ODD_CCW: [u32; 3] = [
    TESS_PRIMITIVE_TRIANGLES,
    TESS_SPACING_FRACTIONAL_ODD,
    TESS_WINDING_CCW,
];

// Capture layout from the existing vertex producer: fixed outputs followed by vec4 user outputs.
const CAPTURE_USER_WORD_BASE: i32 = 16;
const VECTOR_COMPONENTS: i32 = 4;
const CAPTURE_USER_LOCATIONS: i32 =
    (super::variant::vertex::CAPTURE_WORDS as i32 - CAPTURE_USER_WORD_BASE) / VECTOR_COMPONENTS;
// Maxwell exposes the u and v coordinates. Factor limits describe the packed level vectors.
const TESS_COORD_COMPONENTS: i32 = 2;
const TESS_INNER_LEVELS: i32 = 2;
const TESS_OUTER_LEVELS: i32 = 4;

pub(super) fn supported(job: &Job) -> bool {
    !job.arch_sm86 && job.nvn_domain_mode == Some(TRIANGLE_FRACTIONAL_ODD_CCW)
}

pub(super) fn translate_native(
    job: &Job,
    code: &[u8],
    ctx: &Context<'_>,
) -> Result<Translated, Failure> {
    if job.mesh_stage.is_some() || job.tf.is_some() {
        return Err(failure(
            "unported: mesh or transform feedback metadata in a domain shader".into(),
        ));
    }
    let structured = structure_job(job, code).map_err(failure)?;
    if structured.props.definitions.stage != Some(crate::attr::Stage::TessellationEvaluation) {
        return Err(failure(
            "domain diagnostic needs a domain shader header".into(),
        ));
    }
    library::build(&structured, ctx, &options("ds", code, ctx)).map_err(failure)
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

// IDs are (first captured vertex ordinal, original patch ID, reserved, reserved).
// State is (uint4 patch info, float4 outer factors, float4 inner factors). info.x
// is the input control point count. These are shader inputs, not synthesized defaults.
pub(super) fn declarations() -> String {
    let words = super::variant::vertex::CAPTURE_WORDS;
    format!(
        "struct dm_domain_vertex {{ float4 coord; uint4 ids; }};\n\
         struct dm_domain_state {{ uint4 info; float4 outer; float4 inner; }};\n\
         static float dm_domain_cp(device const uint* data, uint base, uint count, uint cp, uint word)\n\
         {{\n    if (cp >= count) return 0.0;\n    return as_type<float>(data[(base + cp) * {words}u + word]);\n}}\n\
         static float dm_domain_user(device const uint* data, uint base, uint count, uint cp, uint loc, uint c)\n\
         {{\n    if (loc >= {CAPTURE_USER_LOCATIONS}u || c >= {VECTOR_COMPONENTS}u) return 0.0;\n    return dm_domain_cp(data, base, count, cp, {CAPTURE_USER_WORD_BASE}u + loc * {VECTOR_COMPONENTS}u + c);\n}}\n"
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
                    IoVariable::TessellationCoord => {
                        ("dm_points[dm_vertex].coord", TESS_COORD_COMPONENTS)
                    }
                    IoVariable::TessellationLevelInner => ("dm_domain.inner", TESS_INNER_LEVELS),
                    _ => ("dm_domain.outer", TESS_OUTER_LEVELS),
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
                    let c = component(self, if user { 3 } else { 2 }, VECTOR_COMPONENTS)?;
                    let word = if user {
                        let l = component(self, 1, CAPTURE_USER_LOCATIONS)?;
                        CAPTURE_USER_WORD_BASE + VECTOR_COMPONENTS * l + c
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
