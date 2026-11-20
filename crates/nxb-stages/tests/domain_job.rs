//! An authored NVN1 package record enters the normal shader producer without changing its opcodes.

#[path = "domain_job/fixture.rs"]
mod fixture;

use nxb_pyjson::Value;
use nxb_shader::job::{JobOptions, job_for};
use nxb_xlate::job::Job;
use nxb_xlate::msl::{Context, translate};

const CODE_START: usize = 0x38 + 0x30;
const CONTROL_LEN: usize = 0x748;
const CTL_MAGIC: [u8; 4] = [0x34, 0x12, 0x76, 0x98];

fn record(sph_stage: u32, mode: [u32; 3]) -> Vec<u8> {
    const PTRUE: u64 = 7 << 16;
    const EXIT: u64 = 0xe300_0000_0007_000f;
    // ALD R0, a[0x2f0], RZ (tessellation coordinate u,v).
    let ald =
        (0b11101111110110u64 << 50) | (255 << 8) | PTRUE | (0x2f0 << 20) | (255 << 39) | (1 << 47);
    // AST a[0x70], R0, RZ (clip position x,y).
    let ast =
        (0b11101111111100u64 << 50) | (255 << 8) | PTRUE | (0x70 << 20) | (255 << 39) | (1 << 47);
    let mut rec = vec![0; CODE_START + 0x50];
    rec[CODE_START..CODE_START + 4].copy_from_slice(&(sph_stage << 10).to_le_bytes());
    for w in [0, ald, ast, EXIT] {
        rec.extend_from_slice(&w.to_le_bytes());
    }
    let mut ctl = vec![0; CONTROL_LEN];
    ctl[..4].copy_from_slice(&CTL_MAGIC);
    for (at, word) in [0x738, 0x73c, 0x744].into_iter().zip(mode) {
        ctl[at..at + 4].copy_from_slice(&word.to_le_bytes());
    }
    rec.extend(ctl);
    rec
}

const TRI_ODD_CCW: [u32; 3] = [4, 0x8e7b, 0x0901];

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
fn original_domain_uses_native_linkage_not_diagnostic_replay() {
    let (j, code) = job("ds.bin", &record(3, TRI_ODD_CCW), None);
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
    assert!(!lib.msl.contains("main0_tess_indexed_domain"));
    let refl = lib.reflection.as_ref().unwrap().to_value();
    assert!(refl.get("domain_replay").is_none());
    assert_eq!(
        refl.get("tess_link")
            .and_then(|v| v.get("float4_locs"))
            .and_then(Value::as_i64),
        Some(0)
    );
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
    for mode in [
        [7, 0x8e7b, 0x0901],
        [4, 0x8e7a, 0x0901],
        [4, 0x8e7b, 0x0900],
    ] {
        let (j, code) = job("ds.bin", &record(3, mode), Some(&fake));
        assert_eq!(j.nvn_domain_mode, Some(mode));
        assert_eq!(j.stage, "ds");
        assert!(!j.arch_sm86);
        assert!(
            translate(&j, &code, &ctx)
                .unwrap_err()
                .detail
                .contains("domain mode")
        );
    }

    let mut no_mode = record(3, TRI_ODD_CCW);
    no_mode.pop();
    let (j, code) = job("ds.bin", &no_mode, Some(&fake));
    assert_eq!(j.nvn_domain_mode, None);
    assert!(
        translate(&j, &code, &ctx)
            .unwrap_err()
            .detail
            .contains("missing domain control metadata")
    );

    let mut nvn2 = record(3, TRI_ODD_CCW);
    let at = nvn2.len() - CONTROL_LEN + 4;
    nvn2[at..at + 4].copy_from_slice(&0xc9u32.to_le_bytes());
    let (j, code) = job("ds.bin", &nvn2, Some(&fake));
    assert!(
        translate(&j, &code, &ctx)
            .unwrap_err()
            .detail
            .contains("NVN2 domain")
    );

    let (j, code) = job("ds.bin", &record(1, TRI_ODD_CCW), None);
    assert!(
        translate(&j, &code, &ctx)
            .unwrap_err()
            .detail
            .contains("domain shader header")
    );

    let (j, code) = job("hs.bin", &record(2, TRI_ODD_CCW), None);
    assert!(
        translate(&j, &code, &ctx)
            .unwrap_err()
            .detail
            .contains("tessellation control")
    );
}

#[test]
fn encoded_domain_and_fragment_publish_only_their_actual_float4_locations() {
    for (package, code, role, locations) in [
        ("vs.bin", fixture::vs_capture(), "capture144", None),
        ("ds.bin", fixture::ds_partial_x(), "domain", Some(1)),
        ("ps.bin", fixture::fs_xyzw(0), "fragment", Some(1)),
        ("ps.bin", fixture::fs_xyzw(1), "fragment", Some(2)),
    ] {
        let (j, code) = job(package, &fixture::record(&code), None);
        let t = translate(&j, &code, &Context::default()).unwrap();
        let l = t.libraries[0].reflection.as_ref().unwrap().to_value();
        let interface = l.get("tess_link").unwrap();
        assert_eq!(interface.get("version").and_then(Value::as_i64), Some(1));
        assert_eq!(interface.get("role").and_then(Value::as_str), Some(role));
        assert_eq!(
            interface.get("float4_locs").and_then(Value::as_i64),
            locations
        );
        if role == "capture144" {
            assert_eq!(
                l.get("entry_geometry_capture").and_then(Value::as_str),
                Some("main0_gs")
            );
        }
    }
}
