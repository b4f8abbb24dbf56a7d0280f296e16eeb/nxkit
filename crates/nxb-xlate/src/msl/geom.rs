//! Geometry shaders as compute libraries.
//!
//! The engine runs a geometry shader as a compute kernel (geometry as compute). One kernel exists per input
//! topology (`_bg0` to `_bg4`). It reads the captured vertices of the draw from `vertex_output`, writes the emitted
//! vertices to `geometry_vb_output` and their indices to `geometry_ib_output`. The vertex shader `_bgv` draws what
//! the kernel wrote. `_b` is a copy of `_bg0`. A passthrough shader also gets the lean kernels `_bl0`, `_bl1` and
//! `_bl3` ([`geom_lean`]). Refer to `docs/translator.md`.

use super::emit::{Options, R};
use super::geom_lean;
use super::geom_reader;
use super::library;
use super::refl::Reflection;
use super::{Context, Library, Translated};
use crate::job::Job;
use crate::translation::msl_entry::structure_job;
use nxb_pyjson::Value;

/// The number of input topologies: points, lines, lines with adjacency, triangles, triangles with adjacency.
pub const INPUT_TOPOLOGIES: i32 = 5;

/// The MSL type of the topology table `trb_data`: the host binds the texture buffer as plain words.
pub const TOPOLOGY_TYPE: &str = "device const uint*";

/// The type of a helper storage buffer. The kernel reads the capture buffer through volatile loads, because copying
/// all capture words at once can fail the pipeline compile of an optimized kernel.
pub fn helper_type(name: &str, write: bool) -> String {
    let constness = if write { "" } else { "const " };
    let volatile = if name == "vertex_output" {
        "volatile "
    } else {
        ""
    };
    format!("{constness}device {volatile}{name}_block*")
}

/// A read of the topology table at an integer coordinate, or `None` when the texture is not that table.
/// The table holds the words of a single channel texture, so the read makes the texel from the word.
pub fn topology_read(tex: &str, coord: &str) -> Option<String> {
    tex.ends_with(".trb_data")
        .then(|| format!("float4(as_type<float>({tex}[{coord}]), 0.0, 0.0, 1.0)"))
}

/// The index of a geometry helper buffer in the reflection (`DMSH_KIND_GEOMETRY`, its `reg`).
pub fn helper_slot(name: &str) -> Option<i64> {
    Some(match name {
        "vertex_output" => 0,
        "geometry_vb_output" | "vb_input" => 1,
        "geometry_ib_output" => 2,
        "vb_info" => 3,
        "trb_data" => 4,
        _ => return None,
    })
}

/// What the geometry shader's header declares about its output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The most vertices one invocation emits.
    pub vertices: u32,
    /// The invocations per primitive.
    pub invocations: u32,
    /// The output topology: 1 points, 6 line strip, 7 triangle strip.
    pub output: u32,
    /// The words of the index buffer per invocation.
    pub index_stride: u32,
}

impl Layout {
    /// The `geometry` object of the reflection, without the stride.
    fn to_value(self, stride: i64) -> Value {
        let int = |x: u32| Value::Int(i64::from(x));
        Value::obj([
            ("vertices", int(self.vertices)),
            ("invocations", int(self.invocations)),
            ("output", int(self.output)),
            ("index_stride", int(self.index_stride)),
            ("stride", Value::Int(stride)),
        ])
    }
}

fn word(code: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(code.get(at..at + 4)?.try_into().ok()?))
}

/// The output bounds of a geometry shader from its header and the input topology (`dmsh_geometry.output_layout`).
/// A passthrough shader takes the vertex count and topology of its input.
pub fn output_layout(code: &[u8], input: i32) -> R<Layout> {
    let (Some(w0), Some(w2), Some(w3), Some(w4), true) = (
        word(code, 0),
        word(code, 8),
        word(code, 12),
        word(code, 16),
        (0..INPUT_TOPOLOGIES).contains(&input),
    ) else {
        return Err("invalid geometry header or input topology".into());
    };
    let (mut vertices, invocations, mut topology) = (w4 & 4095, w2 >> 24, (w3 >> 24) & 15);
    if passthrough(code) {
        let n = input as usize;
        vertices = [1, 2, 2, 3, 3][n];
        topology = [1, 6, 6, 7, 7][n];
    }
    let _ = w0;
    if !matches!(topology, 1 | 6 | 7) || vertices == 0 || invocations == 0 {
        return Err("invalid geometry output declaration".into());
    }
    Ok(Layout {
        vertices,
        invocations,
        output: topology,
        index_stride: vertices * 2,
    })
}

/// The shader is an NV passthrough geometry shader (word 0 bit 24 of its header): the vertices reach the rasterizer
/// unchanged and the shader only decides per primitive.
pub fn passthrough(code: &[u8]) -> bool {
    word(code, 0).is_some_and(|w| w >> 24 & 1 != 0)
}

/// The kernel of one input topology as the library `_bg<topology>`, and the lean kernel `_bl<topology>` when the
/// shader has one.
fn kernel(
    job: &Job,
    code: &[u8],
    ctx: &Context<'_>,
    opt: &Options,
    t: i32,
) -> R<(Library, Option<Library>)> {
    let layout = output_layout(code, t)?;
    let mut j = job.clone();
    j.key = format!("{}_g{t}", job.key);
    j.geometry_topology = Some(t);
    let mut s = structure_job(&j, code)?;
    s.job.key = job.key.clone();
    let stride = s.program.geometry_stride;
    // The lean transform reads the text of the kernel, one declaration to a line.
    let opt = &Options {
        plain_decls: true,
        ..opt.clone()
    };
    let mut done = library::build(&s, ctx, opt)?;
    if done.libraries.len() != 1 {
        return Err("geometry kernel with more than one library".into());
    }
    let mut lib = done.libraries.remove(0);
    let mut r: Reflection = lib
        .reflection
        .take()
        .ok_or("geometry kernel without reflection")?;
    r.key = format!("{}_bg{t}", job.key);
    r.drv_unfillable = Vec::new();
    r.set_extra("geometry", layout.to_value(stride));
    let mut lean = None;
    if passthrough(code) && matches!(t, 0 | 1 | 3) {
        match geom_lean::lean(&lib.msl, stride) {
            Ok(l) => {
                let refl = geom_lean::reflection(&r, &l, t, &job.key, layout.to_value(stride));
                lean = Some(Library {
                    suffix: format!("_bl{t}"),
                    msl: l.msl,
                    reflection: Some(refl),
                });
            }
            Err(e) => r.notes.push(e),
        }
    }
    lib.suffix = format!("_bg{t}");
    lib.reflection = Some(r);
    Ok((lib, lean))
}

/// All libraries of a geometry shader: `_b`, `_bg0` to `_bg4`, `_bgv`, then the lean kernels `_bl0`, `_bl1` and `_bl3`
/// of a passthrough shader.
pub fn translate(job: &Job, code: &[u8], ctx: &Context<'_>, opt: &Options) -> R<Translated> {
    let mut libs = Vec::new();
    let mut lean = Vec::new();
    for t in 0..INPUT_TOPOLOGIES {
        let (full, l) = kernel(job, code, ctx, opt, t)?;
        libs.push(full);
        lean.extend(l);
    }
    let mut base = libs[0].clone();
    base.suffix = "_b".into();
    if let Some(r) = base.reflection.as_mut() {
        r.key = format!("{}_b", job.key);
    }
    libs.insert(0, base);
    libs.push(geom_reader::library(job, code, ctx, opt)?);
    libs.extend(lean);
    Ok(Translated {
        key: job.key.clone(),
        stage: job.stage.clone(),
        libraries: libs,
        ..Translated::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(w0: u32, w2: u32, w3: u32, w4: u32) -> Vec<u8> {
        [w0, 0, w2, w3, w4]
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect()
    }

    #[test]
    fn only_the_capture_buffer_is_volatile() {
        assert_eq!(
            helper_type("vertex_output", false),
            "const device volatile vertex_output_block*"
        );
        assert_eq!(
            helper_type("geometry_vb_output", true),
            "device geometry_vb_output_block*"
        );
    }

    #[test]
    fn the_topology_table_reads_as_a_texel() {
        assert_eq!(
            topology_read("spvDescriptorSet0.trb_data", "v3").as_deref(),
            Some("float4(as_type<float>(spvDescriptorSet0.trb_data[v3]), 0.0, 0.0, 1.0)")
        );
        assert_eq!(topology_read("spvDescriptorSet0.tex0", "v3"), None);
    }

    #[test]
    fn helper_slots_follow_the_host() {
        let slots: Vec<Option<i64>> = [
            "vertex_output",
            "geometry_vb_output",
            "geometry_ib_output",
            "vb_info",
            "trb_data",
            "vb_input",
            "x",
        ]
        .iter()
        .map(|n| helper_slot(n))
        .collect();
        assert_eq!(
            slots,
            [Some(0), Some(1), Some(2), Some(3), Some(4), Some(1), None]
        );
    }

    #[test]
    fn layout_reads_the_header() {
        let c = header(0, 1 << 24, 7 << 24, 6);
        let l = output_layout(&c, 3).expect("layout");
        assert_eq!((l.vertices, l.invocations, l.output), (6, 1, 7));
        assert_eq!(l.index_stride, 12);
    }

    #[test]
    fn passthrough_takes_the_input_shape() {
        let c = header(1 << 24, 1 << 24, 1 << 24, 0);
        let l = output_layout(&c, 4).expect("layout");
        assert_eq!((l.vertices, l.output, l.index_stride), (3, 7, 6));
        let l = output_layout(&c, 0).expect("layout");
        assert_eq!((l.vertices, l.output), (1, 1));
    }

    #[test]
    fn a_bad_header_is_refused() {
        assert!(output_layout(&[0; 8], 0).is_err());
        let c = header(0, 1 << 24, 2 << 24, 6);
        assert!(
            output_layout(&c, 0)
                .expect_err("topology")
                .contains("output declaration")
        );
        let c = header(0, 1 << 24, 7 << 24, 6);
        assert!(output_layout(&c, 5).is_err());
        let c = header(0, 0, 7 << 24, 6);
        assert!(output_layout(&c, 0).is_err());
    }
}
