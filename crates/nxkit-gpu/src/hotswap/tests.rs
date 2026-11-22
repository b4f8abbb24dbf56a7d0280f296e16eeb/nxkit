//! The registry, the checks of a swapped source and the verbs against a real device.

use super::*;

const SOURCE: &str = r#"
#include <metal_stdlib>
using namespace metal;
fragment float4 main0() { return float4(1, 0, 0, 1); }
"#;

fn lib(ctx: &Context, source: &str) -> ShaderLibrary {
    ShaderLibrary::from_source(ctx, "t", source).unwrap()
}

/// A run folder with the reflection and the source of one fragment program `0abc`.
fn run(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nxkit-hotswap-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("shaders")).unwrap();
    std::fs::create_dir_all(d.join(nxkit_abi::shader_src::DIR)).unwrap();
    std::fs::write(
        d.join("shaders/0abc_b.json"),
        r#"{"entry": "main0", "bindings": []}"#,
    )
    .unwrap();
    std::fs::write(
        d.join(nxkit_abi::shader_src::DIR).join("0abc_b.metal"),
        SOURCE,
    )
    .unwrap();
    d
}

#[test]
fn a_key_names_one_file() {
    assert_eq!(valid_key(" 0abc "), Ok("0abc"));
    for bad in ["", "../x", "a/b", "a.metal", "a\\b"] {
        assert!(valid_key(bad).is_err(), "{bad}");
    }
}

#[test]
fn the_functions_the_host_calls_are_checked_by_name() {
    let json = parse(r#"{"entry": "main0", "entry_u": "main0_u", "bindings": []}"#).unwrap();
    let need = required(&json, false).unwrap();
    assert_eq!(need, ["main0", "main0_u"]);
    let have = ["main0".to_string()];
    assert_eq!(missing(&need, &have), ["main0_u"]);
    assert!(required(&json, true).is_err(), "no pull field");
}

#[test]
fn a_metal_error_is_cut_to_its_error_lines() {
    let text = "program_source:3:1: error: unknown type name 'floot4'\nfloot4 x;\n^\nprogram_source:5:1: error: expected ;\n";
    assert_eq!(
        error_summary(text),
        "program_source:3:1: error: unknown type name 'floot4' | program_source:5:1: error: expected ;"
    );
    assert_eq!(error_summary("plain\nmore"), "plain");
}

#[test]
fn the_registry_numbers_each_swap_and_forgets_on_reset() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let mut r = Registry::default();
    let a = r.put("0abc", lib(&ctx, SOURCE));
    let b = r.put("0abc", lib(&ctx, SOURCE));
    assert!(a != 0 && b > a, "a new swap is a new number");
    assert!(r.remove("0abc") && !r.remove("0abc"));
    r.put("0abc", lib(&ctx, SOURCE));
    r.put("0def", lib(&ctx, SOURCE));
    assert_eq!(r.clear(), 2);
}

#[test]
fn dump_load_and_reset_run_against_a_device() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let root = run("verbs");
    let hub = ShaderHub::new(root.join("diag/shaders"));
    assert!(hub.dump("0abc").is_err_and(|e| e.contains("not started")));
    hub.attach(&ctx, &root.join("shaders"), false);
    // dump writes the file once and keeps edits
    let wrote = hub.dump("0abc").unwrap();
    assert!(wrote[0].contains("0abc.metal"));
    assert!(
        hub.dump("0abc")
            .is_err_and(|e| e.contains("keeps your edits"))
    );
    assert_eq!(hub.0.epoch.load(Ordering::Relaxed), 0);
    // an unknown program has no reflection
    assert!(hub.dump("0def").is_err());
    // a source that does not compile names its errors and changes nothing
    let file = hub.dir().join("0abc.metal");
    std::fs::write(&file, "fragment float4 main0() { return floot4(1); }").unwrap();
    let e = hub.load("0abc").unwrap_err();
    assert!(e.contains("does not compile") && e.contains("error"), "{e}");
    assert!(hub.dir().join("0abc.errors.txt").is_file());
    assert_eq!(hub.0.epoch.load(Ordering::Relaxed), 0);
    // a source without the entry of the reflection is refused
    std::fs::write(&file, SOURCE.replace("main0", "other")).unwrap();
    assert!(
        hub.load("0abc")
            .is_err_and(|e| e.contains("lacks the functions main0"))
    );
    // a good source is put in place
    std::fs::write(&file, SOURCE.replace("1, 0, 0, 1", "0, 1, 0, 1")).unwrap();
    assert!(hub.load("0abc").is_ok());
    assert_eq!(hub.0.epoch.load(Ordering::Relaxed), 1);
    assert_eq!(hub.swapped(), ["0abc swap 1"]);
    assert!(hub.reset("0def").is_err());
    assert!(hub.reset("0abc").is_ok());
    assert!(hub.swapped().is_empty());
    assert_eq!(hub.0.epoch.load(Ordering::Relaxed), 2);
    assert!(hub.reset("all").is_ok());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dump_says_how_to_get_a_missing_source() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let root = run("nosrc");
    std::fs::remove_dir_all(root.join(nxkit_abi::shader_src::DIR)).unwrap();
    let hub = ShaderHub::new(root.join("diag/shaders"));
    hub.attach(&ctx, &root.join("shaders"), false);
    let e = hub.dump("0abc").unwrap_err();
    assert!(
        e.contains("NXB_KEEP_MSL=1") && e.contains("xlate-msl"),
        "{e}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn counting_a_frame_without_draws_adds_the_frame_only() {
    // `list` itself waits for a running game, so only the counting is checked.
    let hub = ShaderHub::new(PathBuf::from("/nonexistent"));
    hub.count(&FrameDetail::default());
    let s = hub.state();
    assert_eq!(s.frames, 1);
    assert!(s.counts.is_empty());
}
