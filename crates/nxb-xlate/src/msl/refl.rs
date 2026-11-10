//! The typed reflection of one library (`<key>_b.json` and the other library files). The MSL back end fills it while
//! it declares the resources. Nothing parses MSL text to find a binding. [`Reflection::to_value`] writes the JSON that
//! the engine host reads (`dm/gfx/nvn/nvn_refl.m`). The compiler fields of the stage step are added by the caller.

use super::abi::Kind;
use nxb_pyjson::Value;

/// One member of the argument buffer at `[[buffer(0)]]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    /// The member name in the MSL source.
    pub msl_name: String,
    /// The member type as the host reads it: `texture2d<float>`, `array<texture2d<float>, 32>`, `sampler`,
    /// `texture2d<float, access::read_write>`, `device uint*`, `const device uint*`. The host matches prefixes of it.
    pub msl_type: String,
    /// `[[id(n)]]` of the member.
    pub arg_id: u32,
    /// The binding kind.
    pub kind: Kind,
    /// The slot (`reg`). `None` writes `null`.
    pub reg: Option<i64>,
    /// `drvcb`: the constant bank.
    pub bank: Option<u32>,
    /// `*_ubo`, `*_c2a`, `*_htab`: the byte offset of the handle.
    pub off: Option<u32>,
    /// The other half of a split handle: byte offset.
    pub soff: Option<u32>,
    /// The other half of a split handle: uniform slot (or [`super::abi::SREG_C2`]).
    pub sreg: Option<u32>,
    /// `tex_pool`, `tex_htab`: the element type.
    pub elem: Option<String>,
    /// `sbuf`, `sbuf_ubo`, `gmem_map`, `gmem_log`: the shader writes it.
    pub rw: Option<bool>,
    /// `img`, `img_ubo`: the shader runs image atomics on it.
    pub atomic: bool,
    /// `tex`: the shader reads it as 1D through a wrapped 2D fetch.
    pub fetch1d: bool,
    /// `tex` with `fetch1d`: the shader also queries its width.
    pub fetch1d_size: bool,
    /// A twin sampler for border colours: its bit in the border block.
    pub border_bit: Option<u32>,
    /// `tex`, `img`: the shader multiplies the integer coordinates of this resource by its render scale, or divides a
    /// size query. The host writes the scale of the bound texture to `render_scale[rscale]` of the support buffer.
    pub rscale: Option<u32>,
}

impl Binding {
    /// A binding with the required fields and no options.
    pub fn new(
        msl_name: impl Into<String>,
        msl_type: impl Into<String>,
        arg_id: u32,
        kind: Kind,
    ) -> Binding {
        Binding {
            msl_name: msl_name.into(),
            msl_type: msl_type.into(),
            arg_id,
            kind,
            reg: None,
            bank: None,
            off: None,
            soff: None,
            sreg: None,
            elem: None,
            rw: None,
            atomic: false,
            fetch1d: false,
            fetch1d_size: false,
            border_bit: None,
            rscale: None,
        }
    }

    /// The JSON object of the binding.
    pub fn to_value(&self) -> Value {
        let mut e = Value::obj([
            ("msl_name", Value::str(self.msl_name.clone())),
            ("msl_type", Value::str(self.msl_type.clone())),
            ("arg_id", Value::Int(i64::from(self.arg_id))),
        ]);
        if let Some(b) = self.border_bit {
            e.set("border_bit", Value::Int(i64::from(b)));
        }
        e.set("kind", Value::str(self.kind.as_str()));
        if let Some(b) = self.bank {
            e.set("bank", Value::Int(i64::from(b)));
        } else if !matches!(
            self.kind,
            Kind::C1 | Kind::Support | Kind::Border | Kind::GmemLog
        ) {
            e.set("reg", self.reg.map_or(Value::Null, Value::Int));
        }
        for (k, v) in [("off", self.off), ("soff", self.soff), ("sreg", self.sreg)] {
            if let Some(v) = v {
                e.set(k, Value::Int(i64::from(v)));
            }
        }
        if let Some(r) = self.rscale {
            e.set("rscale", Value::Int(i64::from(r)));
        }
        if let Some(el) = &self.elem {
            e.set("elem", Value::str(el.clone()));
        }
        if let Some(rw) = self.rw {
            e.set("rw", Value::Bool(rw));
        }
        for (k, v) in [
            ("atomic", self.atomic),
            ("fetch1d", self.fetch1d),
            ("fetch1d_size", self.fetch1d_size),
        ] {
            if v {
                e.set(k, Value::Bool(true));
            }
        }
        e
    }
}

/// A vertex input `[[attribute(n)]]`, `n` is the NVN attribute number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VertexInput {
    /// The attribute number.
    pub location: u32,
    /// The MSL type (`float4`, `int4`, `uint4`, ...).
    pub msl_type: String,
}

/// A reference to one uniform word: slot, vec4 index, component.
pub type UniformRef = (u32, u32, u32);

/// The affine transform of a varying from an attribute (`raster.uv_affine`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UvAffine {
    /// The vertex attribute.
    pub attr: u32,
    /// Scale x, offset x, scale y, offset y.
    pub sx: UniformRef,
    /// See `sx`.
    pub ox: UniformRef,
    /// See `sx`.
    pub sy: UniformRef,
    /// See `sx`.
    pub oy: UniformRef,
}

/// The raster facts (`raster`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Raster {
    /// Vertex: the attribute the position is copied from.
    pub pos_attr: Option<u32>,
    /// Vertex: the position's w is 1.
    pub pos_w_one: bool,
    /// Pixel: the shader may discard. `None` writes nothing (the host assumes it may).
    pub discard: Option<bool>,
    /// Pixel: the shader writes the sample mask. `None` writes nothing.
    pub sample_mask: Option<bool>,
    /// Pixel: the shader reads the primitive ID.
    pub primitive_id: bool,
    /// Pixel: the colour targets written. `None` writes nothing (the host assumes all).
    pub colors: Option<Vec<u32>>,
    /// Vertex: varying location to the attribute it copies.
    pub uv_from_attr: Vec<(u32, u32)>,
    /// Vertex: varying location to its affine transform of an attribute.
    pub uv_affine: Vec<(u32, UvAffine)>,
    /// Vertex: varying locations in screen space.
    pub screen: Vec<u32>,
}

impl Raster {
    /// The JSON object.
    pub fn to_value(&self) -> Value {
        let mut r = Value::Object(Vec::new());
        if let Some(a) = self.pos_attr {
            r.set("pos_attr", Value::Int(i64::from(a)));
            r.set(
                "pos_w",
                Value::str(if self.pos_w_one { "one" } else { "attr" }),
            );
        }
        if let Some(d) = self.discard {
            r.set("discard", Value::Int(i64::from(d)));
        }
        if let Some(s) = self.sample_mask {
            r.set("sample_mask", Value::Int(i64::from(s)));
        }
        if self.primitive_id {
            r.set("primitive_id", Value::Int(1));
        }
        if let Some(c) = &self.colors {
            r.set("colors", ints(c));
        }
        if !self.uv_from_attr.is_empty() {
            let o = self
                .uv_from_attr
                .iter()
                .map(|(l, a)| (l.to_string(), Value::Int(i64::from(*a))))
                .collect();
            r.set("uv_from_attr", Value::Object(o));
        }
        if !self.uv_affine.is_empty() {
            let u = |x: &UniformRef| ints(&[x.0, x.1, x.2]);
            let o = self
                .uv_affine
                .iter()
                .map(|(l, a)| {
                    let e = Value::obj([
                        ("attr", Value::Int(i64::from(a.attr))),
                        ("sx", u(&a.sx)),
                        ("ox", u(&a.ox)),
                        ("sy", u(&a.sy)),
                        ("oy", u(&a.oy)),
                    ]);
                    (l.to_string(), e)
                })
                .collect();
            r.set("uv_affine", Value::Object(o));
        }
        if !self.screen.is_empty() {
            r.set("screen", ints(&self.screen));
        }
        r
    }
}

fn ints(v: &[u32]) -> Value {
    Value::List(v.iter().map(|x| Value::Int(i64::from(*x))).collect())
}

/// The reads of one driver bank.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DrvRead {
    /// The bank (0 or 2).
    pub bank: u32,
    /// The byte offsets of the vec4 words read at constant indices, sorted.
    pub vec4_offsets: Vec<u32>,
    /// The shader indexes the bank at run time.
    pub dynamic: bool,
}

/// The driver locations the host cannot fill for a stage (`unfillable` of the engine build).
pub fn unfillable(reads: &[DrvRead], stage: &str) -> Vec<String> {
    use super::abi::*;
    let mut bad = Vec::new();
    for d in reads {
        let mut fill: Vec<(u32, u32)> = if d.bank == 0 {
            [C0_SSBO_VS, C0_SSBO_CS, C0_SSBO_GS, C0_SSBO_PS]
                .iter()
                .map(|b| (*b, b + C0_ROW * C0_SSBO_SLOTS))
                .collect()
        } else {
            vec![
                (C2_TEX, C2_SEPTEX),
                (C2_IMG, C2_IMG + C2_ROW * C2_IMG_SLOTS),
                (C2_SEPTEX, C2_SAMP),
                (C2_SAMP, C2_SAMP + C2_ROW * C2_SEP_SLOTS),
            ]
        };
        if d.bank == 0 && stage == "cs" {
            fill.push((C0_DISPATCH, C0_DISPATCH + 12));
        }
        if d.bank == 0 && stage == "ps" {
            fill.push((C0_SAMPLE_TABLE, C0_SAMPLE_TABLE + C0_SAMPLE_BYTES));
            fill.push((C0_SAMPLE_INFO, C0_SAMPLE_INFO + C0_ROW));
        }
        if d.bank == 0
            && (stage == "ts" || stage == "ms")
            && let Some(b) = c0_ssbo(stage)
        {
            fill.push((b, b + C0_ROW * C0_SSBO_SLOTS));
        }
        if d.dynamic && d.bank == 0 && !(stage == "ps" && d.vec4_offsets.contains(&C0_SAMPLE_INFO))
        {
            bad.push(format!("c{} dynamic index", d.bank));
        }
        for &o in &d.vec4_offsets {
            if !fill.iter().any(|(a, b)| o < *b && *a < o + 16) {
                bad.push(format!("c{}+{o:#x}", d.bank));
            }
        }
    }
    bad
}

/// The reflection of one library file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reflection {
    /// `<key><suffix>`.
    pub key: String,
    /// The chain version V.
    pub chain: String,
    /// The shader key without suffix.
    pub dm_ctl: String,
    /// `<package>#<index>`.
    pub source: String,
    /// The paired shader of the game's program.
    pub pc_paired: String,
    /// `vs`, `ps`, `cs`, `gs`, `ts`, `ms`.
    pub stage: String,
    /// The main entry point (`main0`).
    pub entry: String,
    /// `vertex`, `fragment` or `kernel`.
    pub msl_entry_kind: String,
    /// More entry points by role, in order: `entry_z`, `entry_discard`, `entry_u`, ... and their companion fields.
    pub entries: Vec<(String, Value)>,
    /// The workgroup size of a compute shader.
    pub threadgroup_size: Option<[u32; 3]>,
    /// The c1 segment as hex.
    pub c1: String,
    /// The translator's notes.
    pub notes: Vec<String>,
    /// The driver banks the shader reads (bank 0 and 2), in bank order.
    pub drv_reads: Vec<DrvRead>,
    /// Driver locations the host cannot fill ([`unfillable`]).
    pub drv_unfillable: Vec<String>,
    /// Pixel: the shader reads the pixel position, which it divides by the render scale of the pass
    /// (`render_scale[0]` of the support buffer).
    pub pass_scale: bool,
    /// The argument buffer members.
    pub bindings: Vec<Binding>,
    /// The vertex inputs.
    pub vertex_inputs: Vec<VertexInput>,
    /// The raster facts.
    pub raster: Raster,
    /// Vertex: varying locations padded for the pixel partners.
    pub vs_pad: Vec<u32>,
    /// Further fields in order (`vin`, `pull`, `fetch`, `alpha`, `logic`, `tg_spill`, `first_store`, `geometry`,
    /// `gs_slice`, mesh fields). Each variant writes its own field.
    pub extra: Vec<(String, Value)>,
}

impl Reflection {
    /// Set a further field (replace it when it exists).
    pub fn set_extra(&mut self, k: &str, v: Value) {
        if let Some(e) = self.extra.iter_mut().find(|(n, _)| n == k) {
            e.1 = v;
        } else {
            self.extra.push((k.to_string(), v));
        }
    }

    /// The JSON object without the compiler fields.
    pub fn to_value(&self) -> Value {
        let mut r = Value::obj([
            ("key", Value::str(self.key.clone())),
            ("route", Value::str("B")),
            ("chain", Value::str(self.chain.clone())),
            ("dm_ctl", Value::str(self.dm_ctl.clone())),
            ("source", Value::str(self.source.clone())),
            ("pc_paired", Value::str(self.pc_paired.clone())),
            ("stage", Value::str(self.stage.clone())),
            ("entry", Value::str(self.entry.clone())),
            ("msl_entry_kind", Value::str(self.msl_entry_kind.clone())),
        ]);
        for (k, v) in &self.entries {
            r.set(k, v.clone());
        }
        r.set(
            "threadgroup_size",
            self.threadgroup_size.map_or(Value::Null, |t| ints(&t)),
        );
        r.set("c1", Value::str(self.c1.clone()));
        r.set(
            "notes",
            Value::List(self.notes.iter().map(|n| Value::str(n.clone())).collect()),
        );
        let dr = self
            .drv_reads
            .iter()
            .map(|d| {
                let e = Value::obj([
                    ("vec4_offsets", ints(&d.vec4_offsets)),
                    ("dynamic", Value::Bool(d.dynamic)),
                ]);
                (d.bank.to_string(), e)
            })
            .collect();
        r.set("drv_reads", Value::Object(dr));
        r.set(
            "drv_unfillable",
            Value::List(
                self.drv_unfillable
                    .iter()
                    .map(|n| Value::str(n.clone()))
                    .collect(),
            ),
        );
        if self.pass_scale {
            r.set("pass_scale", Value::Bool(true));
        }
        r.set(
            "bindings",
            Value::List(self.bindings.iter().map(Binding::to_value).collect()),
        );
        r.set(
            "vertex_inputs",
            Value::List(
                self.vertex_inputs
                    .iter()
                    .map(|v| {
                        Value::obj([
                            ("location", Value::Int(i64::from(v.location))),
                            ("msl_type", Value::str(v.msl_type.clone())),
                        ])
                    })
                    .collect(),
            ),
        );
        r.set("raster", self.raster.to_value());
        r.set("vs_pad", ints(&self.vs_pad));
        for (k, v) in &self.extra {
            r.set(k, v.clone());
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_fields() {
        let mut b = Binding::new("fp_t2d_3", "texture2d<float>", 4, Kind::Tex);
        b.reg = Some(3);
        b.fetch1d = true;
        let v = b.to_value();
        assert_eq!(v.get("kind").and_then(Value::as_str), Some("tex"));
        assert_eq!(v.get("reg").and_then(Value::as_i64), Some(3));
        assert_eq!(v.get("fetch1d"), Some(&Value::Bool(true)));
        assert!(v.get("atomic").is_none());
    }

    #[test]
    fn drvcb_has_bank_not_reg() {
        let mut b = Binding::new("fp_c0", "constant uint*", 0, Kind::DrvCb);
        b.bank = Some(0);
        let v = b.to_value();
        assert_eq!(v.get("bank").and_then(Value::as_i64), Some(0));
        assert!(v.get("reg").is_none());
    }

    #[test]
    fn route_is_b() {
        let r = Reflection::default().to_value();
        assert_eq!(r.get("route").and_then(Value::as_str), Some("B"));
    }
}
