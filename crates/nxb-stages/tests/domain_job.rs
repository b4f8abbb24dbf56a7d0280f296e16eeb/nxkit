//! An authored NVN1 package record enters the normal shader producer without changing its opcodes.

#[path = "domain_job/fixture.rs"]
#[allow(
    dead_code,
    reason = "the standalone builder test uses the VS and FS cases"
)]
mod fixture;

use nxb_pyjson::Value;
use nxb_shader::job::{JobOptions, job_for};
use nxb_xlate::job::Job;
use nxb_xlate::msl::{Context, translate};

use fixture::{
    CODE_START, CONTROL_LEN, CTL_MAGIC, CTL_VERSION_OFFSET, NVN2_CTL_VERSION, SPH_DOMAIN, SPH_HULL,
    SPH_VERTEX, TRI_ODD_CCW, coordinate_record,
};

fn job(package: &str, rec: &[u8], facts: Option<&Value>) -> (Job, Vec<u8>) {
    let j = job_for(
        "0123456789abcdef",
        package,
        rec,
        facts,
        "x.bin",
        JobOptions::default(),
    )
    .unwrap();
    let ctl_at = rec.windows(4).position(|w| w == CTL_MAGIC).unwrap();
    let job = Job::from_value(&j.json).unwrap();
    if !job.arch_sm86 {
        assert_eq!(j.code, rec[CODE_START..ctl_at]);
    }
    (job, j.code)
}

#[test]
fn original_domain_uses_the_normal_base_library() {
    let (j, code) = job("ds.bin", &coordinate_record(SPH_DOMAIN, TRI_ODD_CCW), None);
    assert_eq!(j.nvn_domain_mode, Some(TRI_ODD_CCW));
    let t = translate(
        &j,
        &code,
        &Context {
            no_facts: true,
            ..Context::default()
        },
    )
    .unwrap();
    assert_eq!(t.stage, "ds");
    assert_eq!(t.libraries.len(), 1);
    let lib = &t.libraries[0];
    assert_eq!(lib.suffix, "_b");
    assert!(lib.msl.contains("dm_points[dm_vertex].coord[0]"));
    assert!(lib.msl.contains("vertex main0_out main0("));
    assert_eq!(lib.reflection.as_ref().unwrap().stage, "ds");
}

#[test]
fn unsupported_missing_or_spoofed_control_does_not_translate() {
    let ctx = Context::default();
    let fake = Value::obj([
        (
            "nvn_domain_mode",
            Value::List(TRI_ODD_CCW.map(|v| Value::Int(i64::from(v))).to_vec()),
        ),
        ("stage", Value::str("vs")),
        ("arch", Value::str("sm86")),
    ]);
    // A mismatch in any one of primitive, spacing and winding must reject the job.
    for field in 0..TRI_ODD_CCW.len() {
        let mut mode = TRI_ODD_CCW;
        mode[field] ^= 1;
        let (j, code) = job("ds.bin", &coordinate_record(SPH_DOMAIN, mode), Some(&fake));
        assert_eq!(j.nvn_domain_mode, Some(mode));
        assert_eq!(j.stage, "ds");
        assert!(!j.arch_sm86);
        assert!(
            translate(&j, &code, &ctx)
                .unwrap_err()
                .detail
                .contains("tessellation evaluation shader")
        );
    }

    let mut no_mode = coordinate_record(SPH_DOMAIN, TRI_ODD_CCW);
    no_mode.pop();
    let (j, code) = job("ds.bin", &no_mode, Some(&fake));
    assert_eq!(j.nvn_domain_mode, None);
    assert!(
        translate(&j, &code, &ctx)
            .unwrap_err()
            .detail
            .contains("tessellation evaluation shader")
    );

    let mut nvn2 = coordinate_record(SPH_DOMAIN, TRI_ODD_CCW);
    let at = nvn2.len() - CONTROL_LEN + CTL_VERSION_OFFSET;
    nvn2[at..at + 4].copy_from_slice(&NVN2_CTL_VERSION.to_le_bytes());
    let (j, code) = job("ds.bin", &nvn2, Some(&fake));
    assert!(
        translate(&j, &code, &ctx)
            .unwrap_err()
            .detail
            .contains("tessellation evaluation shader")
    );

    let (j, code) = job("ds.bin", &coordinate_record(SPH_VERTEX, TRI_ODD_CCW), None);
    assert!(
        translate(&j, &code, &ctx)
            .unwrap_err()
            .detail
            .contains("domain shader header")
    );

    let (j, code) = job("hs.bin", &coordinate_record(SPH_HULL, TRI_ODD_CCW), None);
    assert!(
        translate(&j, &code, &ctx)
            .unwrap_err()
            .detail
            .contains("tessellation control")
    );
}

#[test]
fn encoded_domain_preserves_real_control_points_and_partial_outputs() {
    let (j, code) = job("ds.bin", &fixture::record(&fixture::ds_partial_x()), None);
    let t = translate(&j, &code, &Context::default()).unwrap();
    let msl = &t.libraries[0].msl;
    for cp in 0..3 {
        for word in 0..4 {
            assert!(msl.contains(&format!("dm_domain.info.x, {cp}u, {word}u)")));
        }
    }
    assert!(msl.contains("out.loc0 = float4(0.0, 0.0, 0.0, 1.0)"));
}

#[test]
fn domain_does_not_accept_mesh_or_transform_feedback_metadata() {
    let record = coordinate_record(SPH_DOMAIN, TRI_ODD_CCW);
    let tf = Value::List(vec![Value::obj([
        ("stride", Value::Int(4)),
        (
            "outputs",
            Value::List(vec![Value::List(vec![Value::Int(0), Value::Int(1)])]),
        ),
    ])]);
    for facts in [
        Value::obj([("mesh_stage", Value::obj([("kind", Value::str("ms"))]))]),
        Value::obj([("tf", tf)]),
    ] {
        let (j, code) = job("ds.bin", &record, Some(&facts));
        let error = translate(&j, &code, &Context::default()).unwrap_err();
        assert_eq!(
            error.detail,
            "unported: mesh or transform feedback metadata in a domain shader"
        );
    }
}
