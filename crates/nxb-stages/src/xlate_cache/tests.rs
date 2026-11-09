use super::*;
use nxb_xlate::msl::{Library, Reflection};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

static DIR_N: AtomicU32 = AtomicU32::new(0);

/// A fresh folder under the build target.
fn dir() -> PathBuf {
    let d: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "..",
        "target",
        "nxb-stages-test",
    ]
    .iter()
    .collect::<PathBuf>()
    .join(format!(
        "xlate-{}-{}",
        std::process::id(),
        DIR_N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn store() -> Store {
    Store::open(&dir()).unwrap()
}

fn job(code: &[u8]) -> Job {
    Job {
        code: code.to_vec(),
        c1: Vec::new(),
        json: Value::obj([
            ("key", Value::str("0123456789abcdef")),
            ("stage", Value::str("ps")),
            ("code", Value::str("0123456789abcdef.bin")),
        ]),
        notes: Vec::new(),
    }
}

fn ctx<'a>(partners: &'a [StageInputs]) -> Context<'a> {
    Context {
        chain: "v1",
        source: "pkg#3".into(),
        pc_paired: "",
        partners,
        no_facts: false,
        ..Context::default()
    }
}

fn varying(location: u32) -> StageInputs {
    StageInputs {
        varyings: vec![Varying {
            location,
            msl_type: "float4".into(),
            interpolation: String::new(),
            read: 0xf,
        }],
    }
}

fn translated(j: &XJob) -> Translated {
    let refl = Reflection {
        key: format!("{}_b", j.key),
        stage: j.stage.clone(),
        ..Reflection::default()
    };
    Translated {
        key: j.key.clone(),
        stage: j.stage.clone(),
        libraries: vec![
            Library {
                suffix: "_b".into(),
                msl: "// main0\n".into(),
                reflection: Some(refl),
            },
            Library {
                suffix: "_bf".into(),
                msl: "// fetch\n".into(),
                reflection: None,
            },
        ],
        inputs: varying(1),
        side: Some(Side {
            alpha: vec!["main0_a".into()],
            logic: vec!["main0_l".into()],
        }),
    }
}

fn counting(runs: &Arc<AtomicU32>) -> Box<Run> {
    let runs = runs.clone();
    Box::new(move |j, _, _| {
        runs.fetch_add(1, Ordering::Relaxed);
        Ok(translated(j))
    })
}

#[test]
fn a_translation_is_kept_and_read_back() {
    let runs = Arc::new(AtomicU32::new(0));
    let x = Xlate::with(store(), "t1".into(), counting(&runs));
    let first = x.translate(&job(b"code"), &ctx(&[])).unwrap();
    let again = x.translate(&job(b"code"), &ctx(&[])).unwrap();
    assert_eq!(first, again);
    assert_eq!(runs.load(Ordering::Relaxed), 1);
    assert_eq!(x.hits.load(Ordering::Relaxed), 1);
    assert_eq!(x.misses.load(Ordering::Relaxed), 1);
    assert_eq!(first.libs.len(), 2);
    assert_eq!(first.libs[1].json, None);
    assert_eq!(first.inputs, varying(1));
    assert_eq!(first.side.as_ref().unwrap().alpha, ["main0_a"]);
    assert_eq!(
        first.libs[0].json.as_ref().unwrap().get("key"),
        Some(&Value::str("0123456789abcdef_b"))
    );
}

#[test]
fn every_input_of_the_key_translates_again() {
    let runs = Arc::new(AtomicU32::new(0));
    let x = Xlate::with(store(), "t1".into(), counting(&runs));
    let p1 = [varying(1)];
    let p2 = [varying(2)];
    x.translate(&job(b"code"), &ctx(&[])).unwrap();
    x.translate(&job(b"other"), &ctx(&[])).unwrap();
    x.translate(&job(b"code"), &ctx(&p1)).unwrap();
    x.translate(&job(b"code"), &ctx(&p2)).unwrap();
    let mut c = ctx(&[]);
    c.chain = "v2";
    x.translate(&job(b"code"), &c).unwrap();
    let mut c = ctx(&[]);
    c.no_facts = true;
    x.translate(&job(b"code"), &c).unwrap();
    let mut c = ctx(&[]);
    c.dual_source = true;
    x.translate(&job(b"code"), &c).unwrap();
    let mut c = ctx(&[]);
    c.fetch_outputs_only = true;
    x.translate(&job(b"code"), &c).unwrap();
    assert_eq!(runs.load(Ordering::Relaxed), 8);
    assert_eq!(x.hits.load(Ordering::Relaxed), 0);
}

#[test]
fn another_translator_identity_does_not_reuse_a_translation() {
    let d = dir();
    let runs = Arc::new(AtomicU32::new(0));
    let a = Xlate::with(Store::open(&d).unwrap(), "t1".into(), counting(&runs));
    a.translate(&job(b"code"), &ctx(&[])).unwrap();
    let b = Xlate::with(Store::open(&d).unwrap(), "t2".into(), counting(&runs));
    b.translate(&job(b"code"), &ctx(&[])).unwrap();
    assert_eq!(runs.load(Ordering::Relaxed), 2);
}

#[test]
fn the_identity_follows_the_chain() {
    assert_eq!(translator_id("v1"), translator_id("v1"));
    assert_ne!(translator_id("v1"), translator_id("v2"));
    assert!(!nxb_xlate::msl::TRANSLATOR_ID.is_empty());
}

#[test]
fn a_refusal_keeps_its_reason_and_is_not_kept() {
    let runs = Arc::new(AtomicU32::new(0));
    let r = runs.clone();
    let x = Xlate::with(
        store(),
        "t1".into(),
        Box::new(move |j, _, _| {
            r.fetch_add(1, Ordering::Relaxed);
            Err(Failure::new("not ported", format!("unported: {}", j.stage)))
        }),
    );
    for _ in 0..2 {
        let f = x.translate(&job(b"code"), &ctx(&[])).unwrap_err();
        assert_eq!(f.reason, "not ported");
        assert_eq!(f.detail, "unported: ps");
    }
    assert_eq!(runs.load(Ordering::Relaxed), 2);
    assert_eq!(x.refused(), [("not ported".to_string(), 2)]);
}

#[test]
fn a_panic_is_a_failed_translation() {
    let x = Xlate::with(
        store(),
        "t1".into(),
        Box::new(|_, _, _| panic!("translator bug")),
    );
    let f = x.translate(&job(b"code"), &ctx(&[])).unwrap_err();
    assert_eq!(f.reason, "translation failed");
    assert_eq!(f.detail, "panic in the Rust translator");
    assert_eq!(x.refused(), [("panic".to_string(), 1)]);
}

#[test]
fn a_job_the_translator_cannot_read_is_a_failed_translation() {
    let runs = Arc::new(AtomicU32::new(0));
    let x = Xlate::with(store(), "t1".into(), counting(&runs));
    let mut j = job(b"code");
    j.json = Value::obj([("stage", Value::str("ps"))]);
    let f = x.translate(&j, &ctx(&[])).unwrap_err();
    assert_eq!(f.reason, "translation failed");
    assert_eq!(runs.load(Ordering::Relaxed), 0);
    assert_eq!(x.refused(), [("error".to_string(), 1)]);
}

#[test]
fn the_partners_reach_the_translator_in_order() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let x = Xlate::with(
        store(),
        "t1".into(),
        Box::new(move |j, _, c| {
            *s.lock().unwrap() = c.partners.to_vec();
            Ok(translated(j))
        }),
    );
    let p = [varying(1), varying(4)];
    x.translate(&job(b"code"), &ctx(&p)).unwrap();
    assert_eq!(*seen.lock().unwrap(), p);
}

#[test]
fn a_damaged_entry_is_translated_again() {
    assert!(Translation::from_text("{\"stage\": 1}").is_none());
    assert!(Translation::from_text("not json").is_none());
}

/// A Maxwell geometry shader that only returns: the 0x50 byte header and one `EXIT`.
fn geometry_code() -> Vec<u8> {
    geometry_code_with(0)
}

/// `geometry_code` with `word0` added to the first header word. Bit 24 is the passthrough flag.
fn geometry_code_with(word0: u32) -> Vec<u8> {
    let mut h = [0u32; 20];
    h[0] = 4 << 10 | word0;
    h[2] = 1 << 24;
    h[3] = 7 << 24;
    h[4] = 6;
    let exit: u64 = 0xe300_0000_0000_0000 | 0x70000 | 0xf;
    let mut c: Vec<u8> = h.iter().flat_map(|w| w.to_le_bytes()).collect();
    for w in [0u64, exit, 0, 0] {
        c.extend(w.to_le_bytes());
    }
    c
}

#[test]
fn a_geometry_job_without_a_topology_keeps_all_its_libraries() {
    let x = Xlate::open(&dir(), "v1").unwrap();
    let mut j = job(&geometry_code());
    j.json.set("stage", Value::str("gs"));
    let t = x.translate(&j, &ctx(&[])).unwrap();
    assert_eq!(t.stage, "gs");
    let names: Vec<&str> = t.libs.iter().map(|l| l.suffix.as_str()).collect();
    assert_eq!(
        names,
        ["_b", "_bg0", "_bg1", "_bg2", "_bg3", "_bg4", "_bgv"]
    );
    for l in &t.libs {
        let key = l.json.as_ref().and_then(|j| j.get("key"));
        assert_eq!(
            key,
            Some(&Value::str(format!("0123456789abcdef{}", l.suffix)))
        );
    }
    let again = x.translate(&j, &ctx(&[])).unwrap();
    assert_eq!(again, t);
    assert_eq!(x.hits.load(Ordering::Relaxed), 1);
}

#[test]
fn a_passthrough_geometry_job_keeps_its_lean_kernels() {
    let x = Xlate::open(&dir(), "v1").unwrap();
    let mut j = job(&geometry_code_with(1 << 24));
    j.json.set("stage", Value::str("gs"));
    let t = x.translate(&j, &ctx(&[])).unwrap();
    let names: Vec<&str> = t.libs.iter().map(|l| l.suffix.as_str()).collect();
    assert_eq!(
        names,
        [
            "_b", "_bg0", "_bg1", "_bg2", "_bg3", "_bg4", "_bgv", "_bl0", "_bl1", "_bl3"
        ]
    );
    let again = x.translate(&j, &ctx(&[])).unwrap();
    assert_eq!(again, t);
    assert_eq!(x.hits.load(Ordering::Relaxed), 1);
}

/// An Ampere compute job whose code holds `HMMA.16816.F16` after the header, or no instruction.
fn ampere_job(hmma: bool) -> Job {
    let mut code = vec![0u8; 0x90];
    if hmma {
        code.extend_from_slice(&0x0000_0004_242e_723c_u64.to_le_bytes());
        code.extend_from_slice(&0x041f_f000_0000_08ff_u64.to_le_bytes());
    }
    let mut j = job(&code);
    j.json.set("arch", Value::str("sm86"));
    j.json.set("stage", Value::str("cs"));
    j
}

static HMMA_2: [Feature; 1] = [Feature {
    name: "hmma",
    revision: 2,
    uses: revision::FEATURES[0].uses,
}];

#[test]
fn a_feature_bump_translates_only_the_shaders_that_use_it() {
    let d = dir();
    let runs = Arc::new(AtomicU32::new(0));
    let a = Xlate::with(Store::open(&d).unwrap(), "t1".into(), counting(&runs));
    a.translate(&ampere_job(true), &ctx(&[])).unwrap();
    a.translate(&ampere_job(false), &ctx(&[])).unwrap();
    assert_eq!(runs.load(Ordering::Relaxed), 2);
    let b =
        Xlate::with(Store::open(&d).unwrap(), "t1".into(), counting(&runs)).with_features(&HMMA_2);
    b.translate(&ampere_job(true), &ctx(&[])).unwrap();
    b.translate(&ampere_job(false), &ctx(&[])).unwrap();
    assert_eq!(
        runs.load(Ordering::Relaxed),
        3,
        "only the HMMA shader again"
    );
    assert_eq!(b.hits.load(Ordering::Relaxed), 1);
}

#[test]
fn a_shader_without_features_keeps_the_key_it_had_before_features() {
    let d = dir();
    let runs = Arc::new(AtomicU32::new(0));
    let none =
        Xlate::with(Store::open(&d).unwrap(), "t1".into(), counting(&runs)).with_features(&[]);
    none.translate(&ampere_job(false), &ctx(&[])).unwrap();
    none.translate(&ampere_job(true), &ctx(&[])).unwrap();
    let x = Xlate::with(Store::open(&d).unwrap(), "t1".into(), counting(&runs));
    x.translate(&ampere_job(false), &ctx(&[])).unwrap();
    x.translate(&ampere_job(true), &ctx(&[])).unwrap();
    assert_eq!(
        runs.load(Ordering::Relaxed),
        3,
        "a new feature translates its users once"
    );
    assert_eq!(x.hits.load(Ordering::Relaxed), 1);
}
