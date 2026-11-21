//! The translator job of one shader (port of `dmsh_build.job_for` with `vertex_ab`, `early_fragment_tests`,
//! `tf_layout`): the code area handed to the translator, its c1 segment, and the job object of `dmsh_xlate`'s job file.

use crate::mesh;
use nxb_pyjson::Value;

/// The control section's magic (`34127698` in the record).
const CTL_MAGIC: [u8; 4] = [0x34, 0x12, 0x76, 0x98];

// Offsets in the NVN1 domain shader control section, not in the shader program header.
const NVN1_TESS_PRIMITIVE_OFFSET: usize = 0x738;
const NVN1_TESS_SPACING_OFFSET: usize = 0x73c;
const NVN1_TESS_WINDING_OFFSET: usize = 0x744;

/// A job that stops the build (Python's `sys.exit('[RED] …')`), or a record the port does not handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobError(pub String);

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for JobError {}

/// A built job.
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    /// The code bytes written to the job's `code` file.
    pub code: Vec<u8>,
    /// The c1 segment.
    pub c1: Vec<u8>,
    /// The job object (its `code` member names the file the caller writes `code` to).
    pub json: Value,
    /// `[RED]` lines printed on the way (`tf_layout`), build continues.
    pub notes: Vec<String>,
}

/// Translation options (`XLATE_DEBUG`, `XLATE_REDUCED`, both on by default).
#[derive(Clone, Copy, Debug)]
pub struct JobOptions {
    /// `debug`.
    pub debug: bool,
    /// `reduced`.
    pub reduced: bool,
}

impl Default for JobOptions {
    fn default() -> Self {
        JobOptions {
            debug: true,
            reduced: true,
        }
    }
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}

/// Python slicing `b[x:y]` with clamping.
fn sl(b: &[u8], x: usize, y: usize) -> &[u8] {
    let (x, y) = (x.min(b.len()), y.min(b.len()));
    if x >= y { &[] } else { &b[x..y] }
}

fn hex(b: &[u8]) -> String {
    nxb_model::to_hex(b)
}

/// (merged code area, c1).
type Merged = (Vec<u8>, Vec<u8>);

/// `vertex_ab`: a VertexA + VertexB program merged into one, or `None`.
fn vertex_ab(key: &str, code: &[u8], ctl: &[u8]) -> Result<Option<Merged>, JobError> {
    if code.len() < 0x80 || ctl.len() < 0x760 || code[..4] != [0x78, 0x56, 0x34, 0x12] {
        return Ok(None);
    }
    let w0 = u32_at(code, 0x30).unwrap_or(0);
    if w0 & 0x1f != 1 || (w0 >> 10) & 0xf != 0 {
        return Ok(None);
    }
    let v: Vec<u32> = (0..6)
        .map(|i| u32_at(ctl, 0x748 + 4 * i).unwrap_or(0))
        .collect();
    let (a_sph, a_len, b_sph, b_len) = (v[0] as usize, v[1] as usize, v[3] as usize, v[4] as usize);
    if a_sph == 0 && a_len == 0 && b_sph == 0 && b_len == 0 {
        return Ok(None);
    }
    let bw0 = if (0x80..=code.len().saturating_sub(0x50)).contains(&b_sph) {
        u32_at(code, b_sph).unwrap_or(0)
    } else {
        0
    };
    let c700 = u32_at(ctl, 0x700).unwrap_or(0) as usize;
    let bad = a_sph != 0x30
        || b_sph != c700 + 0x30
        || bw0 & 0x1f != 1
        || (bw0 >> 10) & 0xf != 1
        || a_len.wrapping_sub(0x50) % 32 != 0
        || b_len.wrapping_sub(0x50) % 32 != 0
        || b_sph + b_len > code.len();
    if bad {
        return Err(JobError(format!(
            "[RED] {key}: vertex shader of shader type 0 without a VertexB part in the expected place (control +0x748 {a_sph:#x} {a_len:#x} {b_sph:#x} {b_len:#x})"
        )));
    }
    let words = |o: usize| -> Vec<u32> {
        (0..20)
            .map(|i| u32_at(code, o + 4 * i).unwrap_or(0))
            .collect()
    };
    let (sa, sb) = (words(a_sph), words(b_sph));
    let mut sph = vec![sb[0] | (sa[0] & !(0xf << 10))];
    for i in 1..4 {
        sph.push((sa[i] & 0xff_ffff).max(sb[i] & 0xff_ffff) | (sb[i] & !0xff_ffff));
    }
    sph.push(sb[4]);
    for i in 5..20 {
        sph.push(sa[i] | sb[i]);
    }
    let mut a = sl(code, a_sph + 0x50, a_sph + a_len).to_vec();
    let b = sl(code, b_sph + 0x50, b_sph + b_len);
    let alen = a.len();
    let mut o = 0;
    while o + 8 <= alen {
        if o % 32 != 0 {
            let mut w = [0u8; 8];
            w.copy_from_slice(&a[o..o + 8]);
            let ins = u64::from_le_bytes(w);
            if ins >> 52 == 0xe30 && ins & 0x1f == 0xf {
                let off = ((alen - (o + 8)) as u64) & 0xff_ffff;
                let new = 0xe24u64 << 52 | off << 20 | (ins & 0xfffff);
                a[o..o + 8].copy_from_slice(&new.to_le_bytes());
            }
        }
        o += 8;
    }
    let mut out = code[..0x30].to_vec();
    for w in sph {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out.extend_from_slice(&a);
    out.extend_from_slice(b);
    Ok(Some((out, sl(code, b_sph + b_len, code.len()).to_vec())))
}

/// `early_fragment_tests(ctl, code)`.
fn early_fragment_tests(ctl: &[u8], code: &[u8]) -> bool {
    ctl.len() > 0x718
        && ctl[0x718] != 0
        && code.len() >= 0x50
        && u32_at(code, 19 * 4).unwrap_or(0) & 2 == 0
}

/// `tf_layout(key, ctl)`: the transform feedback streams, `None` without (or with an unknown layout, noted).
fn tf_layout(key: &str, ctl: &[u8], notes: &mut Vec<String>) -> Option<Value> {
    let strides: Vec<u16> = (0..4)
        .map(|b| u16_at(ctl, 0x6dc + 2 * b).unwrap_or(0))
        .collect();
    if strides.iter().all(|s| *s == 0) {
        return None;
    }
    let mut out = Vec::new();
    for (b, &stride) in strides.iter().enumerate() {
        if stride == 0 {
            out.push(Value::List(vec![]));
            continue;
        }
        let count = usize::from(*ctl.get(0x6e8 + b).unwrap_or(&0));
        let locs = sl(ctl, 0xdc + 0x80 * b, 0xdc + 0x80 * b + count);
        let sizes = sl(ctl, 0x4dc + 0x80 * b, 0x4dc + 0x80 * b + count);
        let sum: u32 = sizes.iter().map(|x| u32::from(*x)).sum();
        if !(1..=128).contains(&count)
            || locs.iter().any(|x| !(0x20..0x40).contains(x))
            || sizes.iter().any(|n| !(1..=4).contains(n))
            || u32::from(stride) != 4 * sum
        {
            let l: Vec<String> = locs.iter().map(u8::to_string).collect();
            let s: Vec<String> = sizes.iter().map(u8::to_string).collect();
            notes.push(format!(
                "[RED] {key}: transform feedback buffer {b} has a layout of unknown form (stride {}, outputs [{}], components [{}]): no capture entry point; see tf_layout",
                stride,
                l.join(", "),
                s.join(", ")
            ));
            return None;
        }
        let outputs = locs
            .iter()
            .zip(sizes)
            .map(|(o, n)| {
                Value::List(vec![
                    Value::Int(i64::from(*o) - 0x20),
                    Value::Int(i64::from(*n)),
                ])
            })
            .collect();
        out.push(Value::obj([
            ("stride", Value::Int(i64::from(stride))),
            ("outputs", Value::List(outputs)),
        ]));
    }
    Some(Value::List(out))
}

/// `job_for(key, name, rec, w)`: `package` is the package file name (its first two letters are the stage),
/// `code_path` the file the caller writes [`Job::code`] to, `facts` the key's answer-table object.
pub fn job_for(
    key: &str,
    package: &str,
    rec: &[u8],
    facts: Option<&Value>,
    code_path: &str,
    opts: JobOptions,
) -> Result<Job, JobError> {
    let Some(ci) = rec.windows(4).position(|w| w == CTL_MAGIC) else {
        return Err(JobError(format!("{key}: no control section in the record")));
    };
    let ctl = &rec[ci..];
    let nvn2 = ctl.len() >= 0x48 && u32_at(ctl, 4) == Some(0xc9);
    let mut code = sl(rec, 0x38, ci).to_vec();
    let mut stage = package.get(..2).unwrap_or("").to_string();
    let mut notes = Vec::new();
    let mut extra: Vec<(&str, Value)> = Vec::new();
    let mut c1;
    if nvn2 {
        // Switch 2 (dmsh_ampere.py): Ampere code, a 0x80-byte SPH for the graphics stages, then the instructions
        // (control +0x38 long), c1 lies between control +0x40 and +0x44 of the code area.
        let (a, e) = (
            u32_at(ctl, 0x40).unwrap_or(0) as usize,
            u32_at(ctl, 0x44).unwrap_or(0) as usize,
        );
        c1 = if a <= e && e <= code.len() {
            code[a..e].to_vec()
        } else {
            Vec::new()
        };
        let area = code.clone();
        code.truncate(u32_at(ctl, 0x38).unwrap_or(0) as usize);
        extra.push(("arch", Value::str("sm86")));
        if stage == "ts" || stage == "ms" {
            // A task or mesh shader is a compute job with its SPH taken off and its stage memory in shared memory.
            let m = mesh::info(key, ctl, &area)?;
            let (lay, size) = m.memory();
            let refused = mesh::own_shared(sl(&code, 0x80, code.len())) || size > mesh::MAX_SHARED;
            extra.push(("mesh_stage", m.to_value(refused)));
            extra.push((
                "stage_memory",
                Value::List(lay.iter().map(|w| Value::Int(*w)).collect()),
            ));
            extra.push((
                "local",
                Value::List(vec![Value::Int(m.threads()), Value::Int(1), Value::Int(1)]),
            ));
            extra.push(("shared", Value::Int(size)));
            code = sl(&code, 0x80, code.len()).to_vec();
            stage = "cs".to_string();
        }
        if stage == "cs" {
            let b = |o: usize| Value::Int(i64::from(ctl.get(o).copied().unwrap_or(0)));
            extra.push((
                "nvn2_cs",
                Value::List(vec![
                    b(0xb1),
                    b(0xb2),
                    b(0xb3),
                    Value::Int(i64::from(u16_at(ctl, 0xba).unwrap_or(0))),
                    Value::Int(i64::from(ctl.get(0xa9).copied().unwrap_or(0)) * 4),
                    b(0xb4),
                    b(0xb5),
                ]),
            ));
        }
    } else {
        let c1o = u32_at(ctl, 0x700).unwrap_or(0) as usize;
        let c1t = u32_at(ctl, 0x704).unwrap_or(0) as usize;
        c1 = sl(rec, 0x38 + c1o, 0x38 + c1t).to_vec();
        if stage == "vs"
            && let Some((c, k)) = vertex_ab(key, &code, ctl)?
        {
            code = c;
            c1 = k;
        }
        if stage != "cs" {
            code = sl(&code, 0x30, code.len()).to_vec();
        } else if code.get(..4) == Some(&[0x79, 0x56, 0x34, 0x12]) {
            if sl(&code, 0x30, 0x100).iter().any(|x| *x != 0) {
                return Err(JobError(format!(
                    "[RED] {key}: compute code area +0x30..+0x100 is not all zero, so the instructions do not start at +0x100"
                )));
            }
            code = sl(&code, 0x100, code.len()).to_vec();
        }
    }
    if !nvn2
        && (stage == "vs" || stage == "gs")
        && let Some(tf) = tf_layout(key, ctl, &mut notes)
    {
        extra.push(("tf", tf));
    }
    if !nvn2 && stage == "ps" && early_fragment_tests(ctl, &code) {
        extra.push(("early_fragment_tests", Value::Bool(true)));
    }
    // The answer table can augment resources, not replace the original domain control.
    let domain_stage = stage == "ds";
    let domain_mode = if domain_stage && !nvn2 {
        [
            NVN1_TESS_PRIMITIVE_OFFSET,
            NVN1_TESS_SPACING_OFFSET,
            NVN1_TESS_WINDING_OFFSET,
        ]
        .into_iter()
        .map(|at| u32_at(ctl, at).map(|word| Value::Int(i64::from(word))))
        .collect::<Option<Vec<_>>>()
        .map(Value::List)
    } else {
        None
    };
    let mesh_job = extra.iter().any(|(k, _)| *k == "mesh_stage");
    let mut j = Value::obj([
        ("key", Value::str(key)),
        ("stage", Value::str(stage)),
        ("code", Value::str(code_path)),
        ("tex", Value::Object(vec![])),
        ("img", Value::Object(vec![])),
        ("attr", Value::Object(vec![])),
        ("fout", Value::Object(vec![])),
        ("c1", Value::str(hex(&c1))),
    ]);
    for (k, v) in extra {
        j.set(k, v);
    }
    j.set("debug", Value::Bool(opts.debug));
    j.set("reduced", Value::Bool(opts.reduced));
    // A task or mesh job takes its threads and memory from its control section, not from the answer table.
    match facts.filter(|_| !mesh_job) {
        None if mesh_job => {}
        None => j.set("no_facts", Value::Bool(true)),
        Some(Value::Object(m)) => {
            for (k, v) in m {
                if k != "nvn_domain_mode"
                    && !(domain_stage && matches!(k.as_str(), "stage" | "arch"))
                {
                    j.set(k, v.clone());
                }
            }
        }
        Some(_) => {}
    }
    if let Some(mode) = domain_mode {
        j.set("nvn_domain_mode", mode);
    }
    Ok(Job {
        code,
        c1,
        json: j,
        notes,
    })
}
