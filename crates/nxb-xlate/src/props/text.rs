//! Parser of the probe's `props` dump (one attempt section, see `Probe.Props` in `tools/xlate-probe/Probe.cs`).

use super::*;
use crate::attr::Stage;
use crate::ir::agg::AggType;
use crate::types::SamplerType;
use std::collections::HashMap;

fn kv(line: &str) -> HashMap<&str, &str> {
    line.split_whitespace()
        .skip(1)
        .filter_map(|t| t.split_once('='))
        .collect()
}

fn int(m: &HashMap<&str, &str>, k: &str) -> Result<i32, String> {
    m.get(k)
        .ok_or_else(|| format!("props: missing {k}"))?
        .parse::<i64>()
        .map(|v| v as i32)
        .map_err(|e| format!("props: {k}: {e}"))
}

fn flag(m: &HashMap<&str, &str>, k: &str) -> Result<bool, String> {
    Ok(int(m, k)? != 0)
}

fn num(s: &str) -> Result<i32, String> {
    s.parse::<i64>()
        .map(|v| v as i32)
        .map_err(|e| format!("props: {s}: {e}"))
}

fn stage_of(v: i32) -> Result<Stage, String> {
    Ok(match v {
        0 => Stage::Compute,
        1 => Stage::Vertex,
        2 => Stage::TessellationControl,
        3 => Stage::TessellationEvaluation,
        4 => Stage::Geometry,
        5 => Stage::Fragment,
        _ => return Err(format!("props: stage {v}")),
    })
}

fn field_at<'a>(f: &[&'a str], i: usize) -> Result<&'a str, String> {
    f.get(i)
        .copied()
        .ok_or_else(|| "props: short line".to_string())
}

impl Props {
    /// Parse one attempt section of a `props` dump (the text after `== attempt`).
    pub fn parse(text: &str) -> Result<Props, String> {
        let mut p = Props::default();
        let mut pending: Option<(bool, usize)> = None; // (is constant buffer, fields left)
        for line in text.lines() {
            if line.starts_with("==") || line.is_empty() {
                continue;
            }
            let (kw, rest) = line.split_once(' ').unwrap_or((line, ""));
            if let Some((is_cb, left)) = pending.as_mut() {
                if kw != "field" {
                    return Err("props: field expected".into());
                }
                let f: Vec<&str> = rest.splitn(3, ' ').collect();
                let field = Field {
                    ty: AggType(num(field_at(&f, 0)?)? as u32),
                    array_length: num(field_at(&f, 1)?)?,
                    name: field_at(&f, 2)?.to_string(),
                };
                let list = if *is_cb {
                    &mut p.properties.constant_buffers
                } else {
                    &mut p.properties.storage_buffers
                };
                list.last_mut()
                    .ok_or("props: no buffer")?
                    .fields
                    .push(field);
                *left -= 1;
                if *left == 0 {
                    pending = None;
                }
                continue;
            }
            match kw {
                "def" => {
                    let m = kv(line);
                    let d = &mut p.definitions;
                    d.stage = Some(stage_of(int(&m, "stage")?)?);
                    let l: Vec<i32> = m
                        .get("local")
                        .unwrap_or(&"")
                        .split(',')
                        .filter_map(|v| v.parse().ok())
                        .collect();
                    if l.len() == 3 {
                        d.compute_local_size = [l[0], l[1], l[2]];
                    }
                    d.gp_passthrough = flag(&m, "gp")?;
                    d.last_in_vertex_pipeline = flag(&m, "lastvtg")?;
                    d.threads_per_input_primitive = int(&m, "tpip")?;
                    d.input_topology = int(&m, "intopo")?;
                    d.output_topology = int(&m, "outtopo")?;
                    d.max_output_vertices = int(&m, "maxout")?;
                    d.dual_source_blend = flag(&m, "dual")?;
                    d.early_z_force = flag(&m, "earlyz")?;
                    d.y_negate_enabled = flag(&m, "ynegate")?;
                    d.origin_upper_left = flag(&m, "originul")?;
                    d.tess_cw = flag(&m, "tesscw")?;
                    d.tess_patch_type = int(&m, "tessptype")?;
                    d.tess_spacing = int(&m, "tessspacing")?;
                    d.ia_indexing = flag(&m, "iaidx")?;
                    d.oa_indexing = flag(&m, "oaidx")?;
                    d.transform_feedback_enabled = flag(&m, "tfb")?;
                    d.supports_scaled_vertex_formats = flag(&m, "scaled")?;
                }
                "imap" => {
                    let f: Vec<&str> = rest.split(' ').collect();
                    let v: Vec<u8> = field_at(&f, 1)?
                        .split(',')
                        .filter_map(|x| x.parse().ok())
                        .collect();
                    if v.len() != 4 {
                        return Err("props: imap".into());
                    }
                    p.definitions.imap_types.push([v[0], v[1], v[2], v[3]]);
                }
                "attrtype" => {
                    let f: Vec<&str> = rest.split(' ').collect();
                    p.definitions
                        .attribute_types
                        .push(num(field_at(&f, 1)?)? as u32);
                }
                "fout" => {
                    let f: Vec<&str> = rest.split(' ').collect();
                    p.definitions
                        .fragment_output_types
                        .push(AggType(num(field_at(&f, 1)?)? as u32));
                }
                "host" => {
                    let m = kv(line);
                    let h = &mut p.host;
                    h.reduced_precision = flag(&m, "reduced")?;
                    h.supports_fragment_shader_interlock = flag(&m, "interlock")?;
                    h.supports_fragment_shader_ordering_intel = flag(&m, "orderingintel")?;
                    h.supports_geometry_shader_passthrough = flag(&m, "gppass")?;
                    h.supports_shader_ballot = flag(&m, "ballot")?;
                    h.supports_shader_barrier_divergence = flag(&m, "barrierdiv")?;
                    h.supports_shader_float64 = flag(&m, "float64")?;
                    h.supports_shader_non_uniform_indexing = flag(&m, "nonuniform")?;
                    h.supports_texture_shadow_lod = flag(&m, "shadowlod")?;
                    h.supports_viewport_mask = flag(&m, "viewportmask")?;
                    h.metal_image_atomics = flag(&m, "metalatomics")?;
                }
                "au" => p.passthrough_attributes = int(&kv(line), "passthrough")?,
                "target" => p.target_api = int(&kv(line), "api")?,
                "physptr" => p.properties.uses_physical_pointers = rest.trim() == "1",
                "cbuf" | "sbuf" => {
                    let f: Vec<&str> = rest.splitn(5, ' ').collect();
                    let def = BufferDef {
                        layout: num(field_at(&f, 0)?)?,
                        set: num(field_at(&f, 1)?)?,
                        binding: num(field_at(&f, 2)?)?,
                        name: field_at(&f, 4)?.to_string(),
                        fields: vec![],
                    };
                    let n = num(field_at(&f, 3)?)? as usize;
                    let is_cb = kw == "cbuf";
                    if is_cb {
                        p.properties.constant_buffers.push(def);
                    } else {
                        p.properties.storage_buffers.push(def);
                    }
                    if n > 0 {
                        pending = Some((is_cb, n));
                    }
                }
                "tex" | "img" => {
                    let f: Vec<&str> = rest.splitn(8, ' ').collect();
                    let def = TextureDef {
                        set: num(field_at(&f, 0)?)?,
                        binding: num(field_at(&f, 1)?)?,
                        array_length: num(field_at(&f, 2)?)?,
                        separate: field_at(&f, 3)? == "1",
                        ty: SamplerType(num(field_at(&f, 4)?)? as u32),
                        format: num(field_at(&f, 5)?)? as usize,
                        flags: num(field_at(&f, 6)?)?,
                        name: field_at(&f, 7)?.to_string(),
                    };
                    if kw == "tex" {
                        p.properties.textures.push(def);
                    } else {
                        p.properties.images.push(def);
                    }
                }
                "lmem" | "smem" => {
                    let f: Vec<&str> = rest.splitn(4, ' ').collect();
                    let def = MemoryDef {
                        id: num(field_at(&f, 0)?)?,
                        ty: AggType(num(field_at(&f, 1)?)? as u32),
                        array_length: num(field_at(&f, 2)?)?,
                        name: field_at(&f, 3)?.to_string(),
                        zero_init: false,
                    };
                    if kw == "lmem" {
                        p.properties.local_memories.push(def);
                    } else {
                        p.properties.shared_memories.push(def);
                    }
                }
                "cbmap" => {
                    let f: Vec<&str> = rest.split(' ').collect();
                    p.cb_slot_to_binding.push((
                        num(field_at(&f, 0)?)?,
                        num(field_at(&f, 1)?)?,
                        num(field_at(&f, 2)?)?,
                    ));
                }
                "helper" => p.helper_functions_mask = num(rest.trim())?,
                "io" => {
                    let f: Vec<&str> = rest.split(' ').collect();
                    p.io_definitions.push((
                        num(field_at(&f, 0)?)? as u32,
                        num(field_at(&f, 1)?)? as u32,
                        num(field_at(&f, 2)?)?,
                        num(field_at(&f, 3)?)?,
                    ));
                }
                _ => return Err(format!("props: unknown line {line}")),
            }
        }
        if p.definitions.stage.is_none() {
            return Err("props: no def line".into());
        }
        Ok(p)
    }
}
