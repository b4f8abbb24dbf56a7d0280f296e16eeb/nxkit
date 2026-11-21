//! The job file: a JSON array of job objects (written by the shader stage, read here).

use nxb_pyjson::{Value, parse};
use std::collections::BTreeMap;

/// One translation job (`XlateJob::job_json`).
#[derive(Clone, Debug)]
pub struct Job {
    pub key: String,
    pub stage: String,
    pub code_path: String,
    pub arch_sm86: bool,
    pub tex: BTreeMap<i32, String>,
    /// image slot to (type, format)
    pub img: BTreeMap<i32, (Option<String>, Option<String>)>,
    pub attr: Vec<(i32, String)>,
    pub fout: Vec<(i32, String)>,
    pub local: Option<Vec<i64>>,
    pub shared: i64,
    pub c1: Vec<u8>,
    pub nvn2_cs: Option<Vec<i32>>,
    pub stage_memory: Option<Vec<i32>>,
    pub geometry_topology: Option<i32>,
    /// Original NVN1 domain primitive, spacing and winding enums, not shader facts.
    pub nvn_domain_mode: Option<[u32; 3]>,
    /// A task or mesh shader, written as a compute job with its SPH taken off.
    pub mesh_stage: Option<MeshStage>,
    /// The transform feedback layout of a vertex or geometry shader. Other stages ignore it.
    pub tf: Option<TransformFeedback>,
    pub early_fragment_tests: bool,
    pub reduced: bool,
    /// `debug` is on unless the job says `false`.
    pub debug: bool,
    pub softfloat64: bool,
}

/// One transform feedback buffer: the user outputs a vertex shader stores to it, interleaved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TfBuffer {
    /// The bytes of one captured vertex. It is four times the components of `outputs`.
    pub stride: u32,
    /// The captured user outputs in buffer order: (output location from 0, components captured from `x` on).
    pub outputs: Vec<(u32, u32)>,
}

/// The transform feedback layout of a job (`tf` of the job object, `tf_layout` of the shader step).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransformFeedback {
    /// The four feedback buffers. `None` is a buffer that the shader does not capture to.
    pub buffers: [Option<TfBuffer>; 4],
}

impl TransformFeedback {
    /// The bytes per vertex of each buffer, 0 for a buffer without capture.
    pub fn strides(&self) -> [u32; 4] {
        self.buffers
            .each_ref()
            .map(|b| b.as_ref().map_or(0, |b| b.stride))
    }
}

/// The most user output locations of a vertex shader.
const TF_LOCATIONS: i64 = 32;

fn tf_buffer(b: usize, v: &Value) -> Result<Option<TfBuffer>, String> {
    let bad = |what: &str| format!("transform feedback buffer {b}: {what}");
    match v {
        Value::Null => return Ok(None),
        Value::List(l) if l.is_empty() => return Ok(None),
        Value::Object(_) => {}
        _ => return Err(bad("not an object")),
    }
    let stride = v
        .get("stride")
        .and_then(Value::as_i64)
        .ok_or_else(|| bad("no stride"))?;
    let list = v
        .get("outputs")
        .and_then(Value::as_list)
        .ok_or_else(|| bad("no outputs"))?;
    let mut outputs = Vec::new();
    for o in list {
        let pair = o.as_list().filter(|p| p.len() == 2);
        let at = |i: usize| pair.and_then(|p| p[i].as_i64());
        let (Some(loc), Some(n)) = (at(0), at(1)) else {
            return Err(bad("an output is not a pair of location and components"));
        };
        if !(0..TF_LOCATIONS).contains(&loc) {
            return Err(bad(&format!(
                "output location {loc} is past the user outputs"
            )));
        }
        if !(1..=4).contains(&n) {
            return Err(bad(&format!("{n} components in one output")));
        }
        outputs.push((loc as u32, n as u32));
    }
    let words: i64 = outputs.iter().map(|o| i64::from(o.1)).sum();
    if outputs.is_empty() || stride != 4 * words {
        return Err(bad(&format!(
            "stride {stride} is not four times the {words} captured components"
        )));
    }
    Ok(Some(TfBuffer {
        stride: stride as u32,
        outputs,
    }))
}

/// The `tf` member of a job: a list of up to four buffers, each `null`, `[]` or `{"stride", "outputs"}`.
/// A job without a buffer has no layout.
fn transform_feedback(v: Option<&Value>) -> Result<Option<TransformFeedback>, String> {
    let l = match v {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::List(l)) if l.len() <= 4 => l,
        Some(_) => return Err("tf is not a list of at most four buffers".into()),
    };
    let mut tf = TransformFeedback::default();
    for (b, e) in l.iter().enumerate() {
        tf.buffers[b] = tf_buffer(b, e)?;
    }
    Ok(tf.buffers.iter().any(Option::is_some).then_some(tf))
}

/// The attribute of the point size in the vertex components of a mesh shader. The job names it `"psiz"`.
pub const POINT_SIZE: i32 = -1;

/// The facts of a task or mesh shader (`mesh_stage` of the job, `dmsh_meshstage.info`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MeshStage {
    /// `ts` or `ms`.
    pub kind: String,
    /// The threads of a workgroup.
    pub threads: u32,
    /// The payload bytes of a task shader.
    pub payload: u32,
    /// Whether a mesh shader has a task shader (`ts` always has its mesh shaders).
    pub task: bool,
    /// The vertices of a primitive of a mesh shader: 1 point, 2 line, 3 triangle. 0 reads as 3.
    pub verts: u32,
    /// The most vertices of a mesh shader.
    pub max_v: u32,
    /// The most primitives of a mesh shader.
    pub max_p: u32,
    /// The vertex components the SPH enables in attribute order: (attribute, component), position as `None` and the
    /// point size as `POINT_SIZE`.
    pub vertex: Vec<(Option<i32>, i32)>,
    /// The per-primitive components: (attribute, component).
    pub primitive: Vec<(i32, i32)>,
    /// Why the build refused the shader, when it did.
    pub refused: Option<String>,
}

fn mesh_stage(v: &Value) -> Option<MeshStage> {
    let n = |k: &str| {
        v.get(k)
            .and_then(Value::as_i64)
            .and_then(|x| u32::try_from(x).ok())
            .unwrap_or(0)
    };
    let comps = |k: &str| -> Vec<(Option<i32>, i32)> {
        let list = v.get(k).and_then(Value::as_list).unwrap_or(&[]);
        list.iter()
            .filter_map(|e| {
                let e = e.as_list()?;
                let first = e.first();
                let at = if first.and_then(Value::as_str) == Some("psiz") {
                    Some(POINT_SIZE)
                } else {
                    first.and_then(Value::as_i64).map(|x| x as i32)
                };
                Some((at, e.get(1)?.as_i64()? as i32))
            })
            .collect()
    };
    let kind = v.get("kind")?.as_str()?.to_string();
    Some(MeshStage {
        threads: n("threads"),
        payload: n("payload"),
        task: !matches!(v.get("task"), Some(Value::Bool(false))),
        verts: n("verts"),
        max_v: n("max_v"),
        max_p: n("max_p"),
        vertex: comps("vertex"),
        primitive: comps("primitive")
            .into_iter()
            .filter_map(|(a, c)| Some((a.filter(|n| *n >= 0)?, c)))
            .collect(),
        refused: v.get("refused").and_then(Value::as_str).map(str::to_string),
        kind,
    })
}

fn int_list(v: &Value) -> Option<Vec<i64>> {
    Some(v.as_list()?.iter().filter_map(Value::as_i64).collect())
}

fn truthy(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::Bool(true)))
}

fn decode_c1(v: Option<&Value>) -> Vec<u8> {
    // the job carries c1 as a list of byte values or a hex string
    match v {
        Some(Value::List(l)) => l
            .iter()
            .filter_map(|x| x.as_i64().map(|b| b as u8))
            .collect(),
        Some(Value::Str(s)) => (0..s.len() / 2)
            .filter_map(|i| u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok())
            .collect(),
        _ => Vec::new(),
    }
}

impl Job {
    /// Read one job object.
    pub fn from_value(v: &Value) -> Result<Job, String> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let key = s("key").ok_or("job without key")?;
        let mut tex = BTreeMap::new();
        if let Some(Value::Object(m)) = v.get("tex") {
            for (k, t) in m {
                let slot = k.parse::<i32>().map_err(|e| format!("tex slot {k}: {e}"))?;
                tex.insert(slot, entry_type(t).unwrap_or_default());
            }
        }
        let mut img = BTreeMap::new();
        if let Some(Value::Object(m)) = v.get("img") {
            for (k, t) in m {
                let slot = k.parse::<i32>().map_err(|e| format!("img slot {k}: {e}"))?;
                let fmt = t.get("fmt").and_then(Value::as_str).map(str::to_string);
                img.insert(slot, (entry_type(t), fmt));
            }
        }
        let pairs = |k: &str| -> Result<Vec<(i32, String)>, String> {
            let mut out = Vec::new();
            if let Some(Value::Object(m)) = v.get(k) {
                for (n, t) in m {
                    let n = n.parse::<i32>().map_err(|e| format!("{k} {n}: {e}"))?;
                    out.push((n, t.as_str().unwrap_or("").to_string()));
                }
            }
            Ok(out)
        };
        let i32s = |k: &str| int_list(v.get(k)?).map(|l| l.into_iter().map(|x| x as i32).collect());
        Ok(Job {
            stage: s("stage").unwrap_or_default(),
            code_path: s("code").unwrap_or_default(),
            arch_sm86: s("arch").as_deref() == Some("sm86"),
            tex,
            img,
            attr: pairs("attr")?,
            fout: pairs("fout")?,
            local: v.get("local").and_then(int_list),
            shared: v.get("shared").and_then(Value::as_i64).unwrap_or(0),
            c1: decode_c1(v.get("c1")),
            nvn2_cs: i32s("nvn2_cs"),
            stage_memory: i32s("stage_memory"),
            geometry_topology: v
                .get("geometry_topology")
                .and_then(Value::as_i64)
                .map(|x| x as i32),
            nvn_domain_mode: v
                .get("nvn_domain_mode")
                .and_then(Value::as_list)
                .and_then(|words| {
                    let [primitive, spacing, winding] = words else {
                        return None;
                    };
                    let word = |v: &Value| u32::try_from(v.as_i64()?).ok();
                    Some([word(primitive)?, word(spacing)?, word(winding)?])
                }),
            mesh_stage: v.get("mesh_stage").and_then(mesh_stage),
            tf: transform_feedback(v.get("tf")).map_err(|e| format!("job {key}: {e}"))?,
            early_fragment_tests: truthy(v.get("early_fragment_tests")),
            reduced: truthy(v.get("reduced")),
            debug: !matches!(v.get("debug"), Some(Value::Bool(false))),
            softfloat64: truthy(v.get("softfloat64")),
            key,
        })
    }
}

/// `n is JsonObject o ? o["type"] : (string)n`
fn entry_type(t: &Value) -> Option<String> {
    match t {
        Value::Object(_) => t.get("type").and_then(Value::as_str).map(str::to_string),
        Value::Str(s) => Some(s.clone()),
        _ => None,
    }
}

/// Parse a job file (a JSON array).
pub fn parse_jobs(text: &str) -> Result<Vec<Job>, String> {
    let v = parse(text).map_err(|e| e.to_string())?;
    v.as_list()
        .ok_or("job file is not an array")?
        .iter()
        .map(Job::from_value)
        .collect()
}

#[cfg(test)]
#[path = "job_tf_tests.rs"]
mod tf_tests;
