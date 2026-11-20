//! Emit inputs for the opt-in tessellation replay regression.
use nxb_pyjson::{Value, dumps_indent};
use nxb_xlate::{job::parse_jobs, msl};
use std::{error::Error, fs, path::Path};

const VS: &str = "0101010101010101";
const PS: &str = "0202020202020202";
const DS: &str = "0303030303030303";
const PT: u64 = 7 << 16;
const EXIT: u64 = 0xe300_0000_0007_000f;
type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn code(stage: u32, ops: &[u64]) -> Vec<u8> {
    let mut header = [0u32; 20];
    header[0] = stage << 10;
    let mut c: Vec<u8> = header.iter().flat_map(|w| w.to_le_bytes()).collect();
    c.extend(0u64.to_le_bytes());
    for (i, w) in ops.iter().copied().chain([EXIT]).enumerate() {
        c.extend(w.to_le_bytes());
        if i % 3 == 2 {
            c.extend(0u64.to_le_bytes());
        }
    }
    c
}

fn ald(rd: u64, offset: u64, patch: bool, components: u64) -> u64 {
    (0b11101111110110u64 << 50)
        | rd
        | (255 << 8)
        | PT
        | (offset << 20)
        | (u64::from(patch) << 31)
        | (255 << 39)
        | ((components - 1) << 47)
}

fn ast(rd: u64, offset: u64, components: u64) -> u64 {
    (0b11101111111100u64 << 50)
        | rd
        | (255 << 8)
        | PT
        | (offset << 20)
        | (255 << 39)
        | ((components - 1) << 47)
}

fn ldc(rd: u64, slot: u64, off: u64) -> u64 {
    (0b11101111100100u64 << 50) | rd | (255 << 8) | PT | (off << 20) | (slot << 36) | (4 << 48)
}

fn ffma32i(rd: u64, ra: u64, scale: f32) -> u64 {
    (0b000011u64 << 58) | rd | (ra << 8) | PT | (u64::from(scale.to_bits()) << 20)
}

fn i2f_u32(rd: u64, rb: u64) -> u64 {
    (0b01011100101110u64 << 50) | rd | (2 << 8) | (2 << 10) | PT | (rb << 20)
}

fn output(dir: &Path, t: msl::Translated, wanted: &str) -> Result<()> {
    let lib = t
        .libraries
        .into_iter()
        .find(|l| l.suffix == wanted)
        .ok_or("no library")?;
    let base = dir.join(format!("{}{}", t.key, lib.suffix));
    fs::write(base.with_extension("metal"), lib.msl)?;
    let reflection = lib.reflection.ok_or("missing reflection")?.to_value();
    fs::write(
        base.with_extension("json"),
        dumps_indent(&reflection, 2) + "\n",
    )?;
    Ok(())
}

fn write_vectors(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    let left = [-0.8f32, -0.4, 0.5, 1., 1., 0., 0., 1.];
    let right = [0.2, -0.4, 0.5, 1., 0., 1., 0., 1.];
    let unused = [-0.1, 0.5, 0.5, 1., 0., 0., 1., 1.];
    let vertices: Vec<_> = [unused, left, unused, unused, right, unused, unused]
        .into_iter()
        .flatten()
        .flat_map(f32::to_le_bytes)
        .collect();
    fs::write(dir.join("vertices.bin"), vertices)?;
    let uniform: Vec<_> = [0.25f32, 1., 0., 0.]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    fs::write(dir.join("domain_uniform_slot2.bin"), uniform)?;
    // The draw starts after the sentinel and subtracts two from each index.
    // The invalid input underflows only the first patch's last control point.
    for (file, indices) in [
        ("indexed.bin", [u16::MAX, 6, 6, 5, 3, 3, 2]),
        ("indexed_invalid_right.bin", [u16::MAX, 6, 6, 0, 3, 3, 2]),
    ] {
        let bytes: Vec<_> = indices.into_iter().flat_map(u16::to_le_bytes).collect();
        fs::write(dir.join(file), bytes)?;
    }
    Ok(())
}

fn shader_job(key: &str, stage: &str) -> Result<nxb_xlate::job::Job> {
    parse_jobs(&format!(r#"[{{"key":"{key}","stage":"{stage}"}}]"#))?
        .into_iter()
        .next()
        .ok_or_else(|| "missing fixture shader job".into())
}

fn write_shaders(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    let ctx = msl::Context::default();
    let vs = shader_job(VS, "vs")?;
    let vertex = code(
        1,
        &[
            ald(0, 0x080, false, 4),
            ast(0, 0x070, 4),
            ald(4, 0x090, false, 4),
            ast(4, 0x0b0, 4),
        ],
    );
    let normal = msl::translate(&vs, &vertex, &ctx).map_err(|e| e.detail)?;
    let indexed =
        msl::indexed_capture::translate_diagnostic(&vs, &vertex, &ctx).map_err(|e| e.detail)?;
    output(dir, normal, "_b")?;
    output(dir, indexed, "_bt")?;

    let ds = shader_job(DS, "ds")?;
    let domain = code(
        3,
        &[
            // Capture control point zero's base position; offset it by (u,v)/2.
            ald(0, 0x070, false, 4),
            ald(4, 0x2f0, false, 2),
            ffma32i(0, 4, 0.5),
            ffma32i(1, 5, 0.5),
            ast(0, 0x070, 4),
            // Red/green are the captured tint at location 3. Blue is a
            // diagnostic of stage-local UBO, original patch ID and raw factor.
            ald(8, 0x0b0, false, 2),
            ldc(10, 5, 0),
            ldc(11, 5, 4),
            ald(4, 0x060, false, 1),
            i2f_u32(4, 4),
            ald(5, 0x010, true, 1),
            ffma32i(10, 4, 0.125),
            ffma32i(10, 5, 0.0625),
            ast(8, 0x0b0, 4),
        ],
    );
    let mut control = vec![0; 0x748];
    for (offset, value) in [(0x738, 4u32), (0x73c, 0x8e7b), (0x744, 0x0901)] {
        control[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    let mode =
        msl::domain::TriangleFractionalOddCcw::from_control(&control).map_err(|e| e.detail)?;
    let t = msl::domain::translate_diagnostic(&ds, &domain, &ctx, &mode).map_err(|e| e.detail)?;
    output(dir, t, "_b")?;

    // The synthetic fragment stage copies the linked color varying.
    fs::write(
        dir.join(format!("{PS}_b.metal")),
        "#include <metal_stdlib>\nusing namespace metal;\nstruct In { float4 tint [[user(locn3)]]; };\nstruct Out { float4 color [[color(0)]]; };\nfragment Out main0(In in [[stage_in]]) { Out o; o.color = in.tint; return o; }\n",
    )?;
    let fs_refl = Value::obj([
        ("stage", Value::str("ps")),
        ("entry", Value::str("main0")),
        ("msl_entry_kind", Value::str("fragment")),
        ("bindings", Value::List(vec![])),
    ]);
    fs::write(
        dir.join(format!("{PS}_b.json")),
        dumps_indent(&fs_refl, 2) + "\n",
    )?;
    Ok(())
}

fn main() -> Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: generate_tess_fixture OUTPUT_DIR")?;
    let dir = Path::new(&path);
    write_vectors(&dir.join("vectors"))?;
    write_shaders(&dir.join("shaders"))
}
