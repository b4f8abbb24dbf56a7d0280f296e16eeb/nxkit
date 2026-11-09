//! A small builder for MIL programs: constants, operations and their output shapes, encoded as the MIL protobuf.
//!
//! Only the operations of the movie network exist (`conv`, `conv_transpose`, `relu`, `add`, `clip`, `pad`,
//! `pixel_shuffle`, `pixel_unshuffle`, `slice_by_size`, `identity`). All tensors are f16, NCHW. The names follow
//! coremltools (`conv_3`, `conv_3_weight_0`, `relu_5`, ...), so a dump of this model reads like a dump of the
//! prototype's. Every constant is an operation of its own, written before the operation that uses it.

use super::blob::{BlobWriter, Entry};
use super::proto::Msg;
use std::collections::BTreeMap;

/// The path the model gives its weights file (`@model_path` is the package's `Data/com.apple.CoreML`).
pub const WEIGHT_PATH: &str = "@model_path/weights/weight.bin";

#[derive(Clone, Copy)]
pub(super) enum Dt {
    Str = 2,
    F16 = 10,
    I32 = 23,
    U32 = 33,
}

/// A tensor of the program: its name and shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tensor {
    /// The SSA name.
    pub name: String,
    /// The shape, NCHW.
    pub shape: Vec<usize>,
}

/// An operation of the program, without the constants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpInfo {
    /// The MIL operation type.
    pub kind: &'static str,
    /// The name of its output.
    pub output: String,
    /// The shape of its output.
    pub shape: Vec<usize>,
}

/// The arguments of a convolution.
pub struct Conv<'a> {
    /// f16 bits `[cout][cin / groups][kh][kw]`.
    pub weight: &'a [u16],
    /// The shape of `weight`.
    pub wshape: [usize; 4],
    /// f16 bits `[cout]`, if the convolution has a bias.
    pub bias: Option<&'a [u16]>,
    /// Stride `[h, w]`.
    pub strides: [i32; 2],
    /// Padding `[top, bottom, left, right]`; `None` is `valid`.
    pub pad: Option<[i32; 4]>,
    /// Groups.
    pub groups: i32,
    /// Write the weight as an immediate value in the model instead of an entry of the weight file (for tiny weights).
    pub immediate: bool,
}

/// The program under construction.
#[derive(Debug, Default)]
pub struct Builder {
    ops: Vec<Msg>,
    infos: Vec<OpInfo>,
    blob: BlobWriter,
    counts: BTreeMap<&'static str, usize>,
}

fn bytes_of(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|h| h.to_le_bytes()).collect()
}

pub(super) fn value_type(dt: Dt, shape: &[usize], scalar: bool) -> Msg {
    let mut t = Msg::new();
    t.uint(1, dt as u64);
    if !scalar {
        t.uint(2, shape.len() as u64);
        for &d in shape {
            let mut c = Msg::new();
            c.uint(1, d as u64);
            let mut dim = Msg::new();
            dim.msg(1, &c);
            t.msg(3, &dim);
        }
    }
    let mut v = Msg::new();
    v.msg(1, &t);
    v
}

fn named(name: &str, ty: &Msg) -> Msg {
    let mut n = Msg::new();
    n.string(1, name).msg(2, ty);
    n
}

fn string_value(s: &str) -> Msg {
    let mut rep = Msg::new();
    rep.string(1, s);
    let mut tv = Msg::new();
    tv.msg(4, &rep);
    let mut imm = Msg::new();
    imm.msg(1, &tv);
    let mut v = Msg::new();
    v.msg(2, &value_type(Dt::Str, &[], true)).msg(3, &imm);
    v
}

impl Builder {
    /// An empty program.
    #[must_use]
    pub fn new() -> Builder {
        Builder::default()
    }

    fn next(&mut self, kind: &'static str) -> usize {
        let n = self.counts.entry(kind).or_insert(0);
        let at = *n;
        *n += 1;
        at
    }

    fn emit_const(&mut self, name: &str, dt: Dt, shape: &[usize], scalar: bool, value: &Msg) {
        let ty = value_type(dt, shape, scalar);
        let mut full = Msg::new();
        full.msg(2, &ty).merge(value);
        let mut op = Msg::new();
        op.string(1, "const")
            .msg(3, &named(name, &ty))
            .map_entry(5, "val", &full)
            .map_entry(5, "name", &string_value(name));
        self.ops.push(op);
    }

    fn immediate(tensor: &Msg) -> Msg {
        let mut imm = Msg::new();
        imm.msg(1, tensor);
        let mut v = Msg::new();
        v.msg(3, &imm);
        v
    }

    fn c_ints(&mut self, name: &str, vals: &[i32]) {
        let mut rep = Msg::new();
        rep.packed_i32(1, vals);
        let mut tv = Msg::new();
        tv.msg(2, &rep);
        self.emit_const(
            name,
            Dt::I32,
            &[vals.len()],
            false,
            &Builder::immediate(&tv),
        );
    }

    fn c_int(&mut self, name: &str, v: i32) {
        let mut rep = Msg::new();
        rep.packed_i32(1, &[v]);
        let mut tv = Msg::new();
        tv.msg(2, &rep);
        self.emit_const(name, Dt::I32, &[], true, &Builder::immediate(&tv));
    }

    fn c_str(&mut self, name: &str, s: &str) {
        self.emit_const(name, Dt::Str, &[], true, &string_value(s));
    }

    fn c_u32(&mut self, name: &str, v: u32) {
        let mut rep = Msg::new();
        rep.bytes(1, &v.to_le_bytes());
        let mut tv = Msg::new();
        tv.msg(7, &rep);
        self.emit_const(name, Dt::U32, &[], true, &Builder::immediate(&tv));
    }

    fn c_f16(&mut self, name: &str, bits: u16) {
        self.c_f16_imm(name, &[], bits.to_le_bytes().as_ref(), true);
    }

    fn c_f16_imm(&mut self, name: &str, shape: &[usize], bytes: &[u8], scalar: bool) {
        let mut rep = Msg::new();
        rep.bytes(1, bytes);
        let mut tv = Msg::new();
        tv.msg(7, &rep);
        self.emit_const(name, Dt::F16, shape, scalar, &Builder::immediate(&tv));
    }

    fn c_f16_blob(&mut self, name: &str, shape: &[usize], data: &[u16]) {
        debug_assert_eq!(data.len(), shape.iter().product::<usize>());
        let offset = self.blob.add(name, &bytes_of(data));
        let mut bf = Msg::new();
        bf.string(1, WEIGHT_PATH).uint(2, offset as u64);
        let mut v = Msg::new();
        v.msg(5, &bf);
        self.emit_const(name, Dt::F16, shape, false, &v);
    }

    fn op(&mut self, kind: &'static str, inputs: &[(&str, &str)], out: &str, shape: &[usize]) {
        let mut op = Msg::new();
        op.string(1, kind);
        for (key, arg) in inputs {
            let mut bind = Msg::new();
            bind.string(1, arg);
            let mut a = Msg::new();
            a.msg(1, &bind);
            op.map_entry(2, key, &a);
        }
        op.msg(3, &named(out, &value_type(Dt::F16, shape, false)))
            .map_entry(5, "name", &string_value(out));
        self.ops.push(op);
        self.infos.push(OpInfo {
            kind,
            output: out.to_owned(),
            shape: shape.to_vec(),
        });
    }

    fn tensor(name: String, shape: Vec<usize>) -> Tensor {
        Tensor { name, shape }
    }

    /// `conv`.
    pub fn conv(&mut self, x: &Tensor, c: &Conv<'_>) -> Tensor {
        let n = format!("conv_{}", self.next("conv"));
        let [cout, _, kh, kw] = c.wshape;
        if c.immediate {
            self.c_f16_imm(
                &format!("{n}_weight_0"),
                &c.wshape,
                &bytes_of(c.weight),
                false,
            );
        } else {
            self.c_f16_blob(&format!("{n}_weight_0"), &c.wshape, c.weight);
        }
        if let Some(b) = c.bias {
            self.c_f16_blob(&format!("{n}_bias_0"), &[cout], b);
        }
        self.c_ints(&format!("{n}_strides_0"), &c.strides);
        self.c_str(
            &format!("{n}_pad_type_0"),
            if c.pad.is_some() { "custom" } else { "valid" },
        );
        let pad = c.pad.unwrap_or([0; 4]);
        self.c_ints(&format!("{n}_pad_0"), &pad);
        self.c_ints(&format!("{n}_dilations_0"), &[1, 1]);
        self.c_int(&format!("{n}_groups_0"), c.groups);
        let ext = |size: usize, p: (i32, i32), k: usize, s: i32| {
            let total = size as i64 + i64::from(p.0) + i64::from(p.1) - k as i64;
            (total / i64::from(s)) as usize + 1
        };
        let h = ext(x.shape[2], (pad[0], pad[1]), kh, c.strides[0]);
        let w = ext(x.shape[3], (pad[2], pad[3]), kw, c.strides[1]);
        let mut inputs = vec![
            ("x", x.name.clone()),
            ("weight", format!("{n}_weight_0")),
            ("strides", format!("{n}_strides_0")),
            ("pad_type", format!("{n}_pad_type_0")),
            ("pad", format!("{n}_pad_0")),
            ("dilations", format!("{n}_dilations_0")),
            ("groups", format!("{n}_groups_0")),
        ];
        if c.bias.is_some() {
            inputs.insert(2, ("bias", format!("{n}_bias_0")));
        }
        let refs: Vec<(&str, &str)> = inputs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let shape = vec![x.shape[0], cout, h, w];
        self.op("conv", &refs, &n, &shape);
        Builder::tensor(n, shape)
    }

    /// `conv_transpose`, 2x2, stride 2, `valid`, no bias. `weight` is `[cin][cout][2][2]`.
    pub fn conv_transpose2(&mut self, x: &Tensor, weight: &[u16], cout: usize) -> Tensor {
        let k = self.next("conv_transpose");
        let n = format!("conv_transpose_{k}");
        let out = format!("{n}_has_output_shape");
        let cin = x.shape[1];
        self.c_f16_blob(&format!("{n}_weight_0"), &[cin, cout, 2, 2], weight);
        self.c_ints(&format!("{n}_strides_0"), &[2, 2]);
        self.c_str(&format!("{n}_pad_type_0"), "valid");
        self.c_ints(&format!("{n}_pad_0"), &[0; 4]);
        self.c_ints(&format!("{n}_dilations_0"), &[1, 1]);
        self.c_int(&format!("{n}_groups_0"), 1);
        let shape = vec![x.shape[0], cout, x.shape[2] * 2, x.shape[3] * 2];
        let dims: Vec<i32> = shape.iter().map(|&d| d as i32).collect();
        self.c_ints(&format!("{n}_has_output_shape_output_shape_0"), &dims);
        let names: Vec<String> = [
            "weight",
            "strides",
            "pad",
            "pad_type",
            "output_shape",
            "groups",
            "dilations",
        ]
        .iter()
        .map(|a| {
            if *a == "output_shape" {
                format!("{n}_has_output_shape_output_shape_0")
            } else {
                format!("{n}_{a}_0")
            }
        })
        .collect();
        let inputs = [
            ("x", x.name.as_str()),
            ("weight", names[0].as_str()),
            ("strides", names[1].as_str()),
            ("pad", names[2].as_str()),
            ("pad_type", names[3].as_str()),
            ("output_shape", names[4].as_str()),
            ("groups", names[5].as_str()),
            ("dilations", names[6].as_str()),
        ];
        self.op("conv_transpose", &inputs, &out, &shape);
        Builder::tensor(out, shape)
    }

    /// `relu`.
    pub fn relu(&mut self, x: &Tensor) -> Tensor {
        let n = format!("relu_{}", self.next("relu"));
        self.op("relu", &[("x", x.name.as_str())], &n, &x.shape);
        Builder::tensor(n, x.shape.clone())
    }

    /// `add` of two tensors of the same shape.
    pub fn add(&mut self, x: &Tensor, y: &Tensor) -> Tensor {
        let n = format!("add_{}", self.next("add"));
        self.op(
            "add",
            &[("x", x.name.as_str()), ("y", y.name.as_str())],
            &n,
            &x.shape,
        );
        Builder::tensor(n, x.shape.clone())
    }

    /// `add` of a per-channel bias `[1, c, 1, 1]` (a weight constant) to `x`.
    pub fn add_bias(&mut self, x: &Tensor, bias: &[u16]) -> Tensor {
        let n = format!("add_{}", self.next("add"));
        let y = format!("{n}_y_0");
        self.c_f16_blob(&y, &[1, bias.len(), 1, 1], bias);
        self.op(
            "add",
            &[("y", y.as_str()), ("x", x.name.as_str())],
            &n,
            &x.shape,
        );
        Builder::tensor(n, x.shape.clone())
    }

    /// `clip` to `[alpha, beta]`, both f16 bits.
    pub fn clip(&mut self, x: &Tensor, alpha: u16, beta: u16) -> Tensor {
        let n = format!("clip_{}", self.next("clip"));
        self.c_f16(&format!("{n}_alpha_0"), alpha);
        self.c_f16(&format!("{n}_beta_0"), beta);
        let (a, b) = (format!("{n}_alpha_0"), format!("{n}_beta_0"));
        self.op(
            "clip",
            &[
                ("x", x.name.as_str()),
                ("beta", b.as_str()),
                ("alpha", a.as_str()),
            ],
            &n,
            &x.shape,
        );
        Builder::tensor(n, x.shape.clone())
    }

    /// `pad` with `mode = "replicate"` by `[top, bottom, left, right]`.
    pub fn pad_replicate(&mut self, x: &Tensor, pad: [i32; 4]) -> Tensor {
        let k = self.next("pad");
        let n = format!("pad_{k}");
        let out = format!("{n}_cast_fp16");
        self.c_ints(&format!("{n}_pad_0"), &pad);
        self.c_str(&format!("{n}_mode_0"), "replicate");
        self.c_f16(&format!("{n}_constant_val_0_to_fp16"), 0);
        let (p, m, c) = (
            format!("{n}_pad_0"),
            format!("{n}_mode_0"),
            format!("{n}_constant_val_0_to_fp16"),
        );
        let shape = vec![
            x.shape[0],
            x.shape[1],
            x.shape[2] + (pad[0] + pad[1]) as usize,
            x.shape[3] + (pad[2] + pad[3]) as usize,
        ];
        self.op(
            "pad",
            &[
                ("x", x.name.as_str()),
                ("pad", p.as_str()),
                ("mode", m.as_str()),
                ("constant_val", c.as_str()),
            ],
            &out,
            &shape,
        );
        Builder::tensor(out, shape)
    }

    /// `pixel_unshuffle` (space to depth): channel `c * f * f + dy * f + dx`.
    pub fn pixel_unshuffle(&mut self, x: &Tensor, f: usize) -> Tensor {
        let n = format!("pixel_unshuffle_{}", self.next("pixel_unshuffle"));
        let d = format!("{n}_downscale_factor_0");
        self.c_u32(&d, f as u32);
        let shape = vec![
            x.shape[0],
            x.shape[1] * f * f,
            x.shape[2] / f,
            x.shape[3] / f,
        ];
        self.op(
            "pixel_unshuffle",
            &[("x", x.name.as_str()), ("downscale_factor", d.as_str())],
            &n,
            &shape,
        );
        Builder::tensor(n, shape)
    }

    /// `pixel_shuffle` (depth to space): output channel `c` takes input channel `c * f * f + dy * f + dx`.
    pub fn pixel_shuffle(&mut self, x: &Tensor, f: usize) -> Tensor {
        let n = format!("pixel_shuffle_{}", self.next("pixel_shuffle"));
        let u = format!("{n}_upscale_factor_0");
        self.c_int(&u, f as i32);
        let shape = vec![
            x.shape[0],
            x.shape[1] / (f * f),
            x.shape[2] * f,
            x.shape[3] * f,
        ];
        self.op(
            "pixel_shuffle",
            &[("x", x.name.as_str()), ("upscale_factor", u.as_str())],
            &n,
            &shape,
        );
        Builder::tensor(n, shape)
    }

    /// `slice_by_size` from `begin` with `size`.
    pub fn slice_by_size(&mut self, x: &Tensor, begin: [i32; 4], size: [usize; 4]) -> Tensor {
        let n = format!("slice_by_size_{}", self.next("slice_by_size"));
        let (b, s) = (format!("{n}_begin_0"), format!("{n}_size_0"));
        self.c_ints(&b, &begin);
        let dims: Vec<i32> = size.iter().map(|&d| d as i32).collect();
        self.c_ints(&s, &dims);
        self.op(
            "slice_by_size",
            &[
                ("x", x.name.as_str()),
                ("begin", b.as_str()),
                ("size", s.as_str()),
            ],
            &n,
            &size,
        );
        Builder::tensor(n, size.to_vec())
    }

    /// `identity` with the name of a function output.
    pub fn identity(&mut self, x: &Tensor, name: &str) -> Tensor {
        self.op("identity", &[("x", x.name.as_str())], name, &x.shape);
        Builder::tensor(name.to_owned(), x.shape.clone())
    }

    /// The finished operations (constants included, in order), the operation table and the weight file.
    #[must_use]
    pub fn finish(self) -> (Vec<Msg>, Vec<OpInfo>, Vec<u8>, Vec<Entry>) {
        let (weights, entries) = self.blob.finish();
        (self.ops, self.infos, weights, entries)
    }
}
