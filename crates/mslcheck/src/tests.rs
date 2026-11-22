use super::*;

const GRADIENT: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct Args { texture3d<float> tex [[id(0)]]; sampler smp [[id(1)]]; };
fragment float4 main0(constant Args& a [[buffer(0)]]) {
    return a.tex.sample(a.smp, float3(0.25f, 0.5f, 0.75f),
        gradient3d(float3(0.125f, 0.0f, 0.0f), float3(0.0f, 0.125f, 0.25f)));
}
"#;

fn options(sema: bool) -> Options {
    Options {
        files: vec![],
        jobs: 1,
        json: true,
        stats: false,
        quiet: false,
        lexdump: None,
        sema,
    }
}

#[test]
fn semantic_rejection_is_not_parser_rejection() {
    let parser = msl_parse::check_source(GRADIENT);
    assert!(parser.accepted(), "{}", parser.detail());
    let result = check(parser, true);
    assert_eq!(
        result.semantic,
        Some(Err("unsupported:sample-option:gradient3d".into()))
    );
    let (line, ok, err) = render(
        Path::new("authored.metal"),
        &Ok(result),
        0.0,
        &options(true),
    );
    assert!(!ok && !err);
    assert!(line.contains("\"phase\": \"sema\""));
    assert!(line.contains("unsupported:sample-option:gradient3d"));
}

#[test]
fn default_parser_only_schema_and_acceptance_stay_unchanged() {
    let result = check(msl_parse::check_source(GRADIENT), false);
    assert!(result.semantic.is_none());
    let (line, ok, err) = render(
        Path::new("authored.metal"),
        &Ok(result),
        0.0,
        &options(false),
    );
    assert!(ok && !err);
    assert!(!line.contains("\"phase\""));
    assert!(line.contains("\"reasons\": {}"));
}

#[test]
fn supported_source_passes_both_checks() {
    let result = check(
        msl_parse::check_source("fragment float4 main0() { return float4(1.0f); }"),
        true,
    );
    assert!(result.parser.accepted(), "{}", result.parser.detail());
    assert_eq!(result.semantic, Some(Ok(())));
}

#[test]
fn parser_rejection_never_enters_semantics() {
    let result = check(
        msl_parse::check_source("fragment float4 main0() { return absent_call(); }"),
        true,
    );
    assert!(!result.parser.accepted());
    assert!(result.semantic.is_none());
    let (line, ok, err) = render(
        Path::new("authored.metal"),
        &Ok(result),
        0.0,
        &options(true),
    );
    assert!(!ok && !err);
    assert!(line.contains("\"phase\": \"parse\""));
}

#[test]
fn unreadable_input_remains_an_input_error() {
    let error = Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "missing fixture",
    ));
    let (line, ok, err) = render(Path::new("missing.metal"), &error, 0.0, &options(true));
    assert!(!ok && err);
    assert!(line.contains("\"error\""));
}
