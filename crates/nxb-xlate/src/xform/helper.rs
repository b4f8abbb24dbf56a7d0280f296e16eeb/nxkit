//! `HelperFunctionManager` (the functions the corpus needs) and `Function`.
use crate::cfg::Cfg;
use crate::ir::{Block, Inst, IoVariable, Ir, NodeId, Opd, StorageKind};
use crate::translation::props::ShaderStage;
use std::collections::HashMap;

type R<T> = Result<T, String>;

/// `Function`.
#[derive(Clone, Debug)]
pub struct Function {
    pub blocks: Vec<Block>,
    pub name: String,
    pub returns_value: bool,
    pub in_arguments: i32,
    pub out_arguments: i32,
}

/// `HelperFunctionName` (the values that are generated here).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum HelperName {
    ConvertDoubleToFloat = 1,
    ConvertFloatToDouble = 2,
    Shuffle = 8,
    ShuffleDown = 9,
    ShuffleUp = 10,
    ShuffleXor = 11,
    TexelFetchScale = 12,
    TextureSizeUnscale = 13,
}

/// A texture or image whose coordinates or size the render scale changes. The reflection tells the host which scale to
/// write for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScaleUse {
    /// An image, not a texture.
    pub image: bool,
    /// The binding of the resource.
    pub binding: i64,
    /// The descriptor index the helper function reads. The scale is `render_scale[1 + index]`.
    pub index: i32,
}

const FIELD_FRAGMENT_RENDER_SCALE_COUNT: i32 = 4;
const FIELD_RENDER_SCALE: i32 = 5;

/// `HelperFunctionManager`: `functions[0]` is the slot of the main function (filled in by the caller).
pub struct Hfm {
    pub functions: Vec<Option<Function>>,
    pub(crate) ids: HashMap<i32, i32>,
    stage: ShaderStage,
    /// The resources that a scale helper was called for, each once.
    pub scaled: Vec<ScaleUse>,
}

impl Hfm {
    pub fn new(function_count: usize, stage: ShaderStage) -> Hfm {
        Hfm {
            functions: vec![None; function_count],
            ids: HashMap::new(),
            stage,
            scaled: Vec::new(),
        }
    }

    /// Notes that a scale helper reads the scale of a resource.
    pub fn note_scaled(&mut self, u: ScaleUse) {
        if !self.scaled.contains(&u) {
            self.scaled.push(u);
        }
    }

    pub fn add_function(&mut self, f: Function) -> i32 {
        self.functions.push(Some(f));
        self.functions.len() as i32 - 1
    }

    pub fn get_or_create_function_id(&mut self, ir: &mut Ir, name: HelperName) -> R<i32> {
        if let Some(id) = self.ids.get(&(name as i32)) {
            return Ok(*id);
        }
        let f = match name {
            HelperName::TexelFetchScale => self.texel_fetch_scale(ir)?,
            HelperName::TextureSizeUnscale => self.texture_size_unscale(ir)?,
            HelperName::ConvertDoubleToFloat => super::softfloat::convert_double_to_float(ir)?,
            HelperName::ConvertFloatToDouble => super::softfloat::convert_float_to_double(ir)?,
            _ => return Err("helper function needs its own parameters".into()),
        };
        let id = self.add_function(f);
        self.ids.insert(name as i32, id);
        Ok(id)
    }

    fn texel_fetch_scale(&self, ir: &mut Ir) -> R<Function> {
        let mut b = B::new(ir);
        let input = b.ir.argument(0);
        let sampler_index = b.ir.argument(1);
        let index = self.scale_index(&mut b, sampler_index);
        let scale = b.load_cb(index, FIELD_RENDER_SCALE);
        let one = b.ir.const_f(1.0);
        let scale_is_one = b.op(fp(Inst::CompareEqual), &[scale, one]);
        let lbl = b.ir.label();
        b.branch_if_false(lbl, scale_is_one);
        b.ret(Some(input));
        b.mark_label(lbl);
        let in_args = if self.stage == ShaderStage::Fragment {
            let zero = b.ir.const_f(0.0);
            let lt0 = b.op(fp(Inst::CompareLess), &[scale, zero]);
            let lbl2 = b.ir.label();
            b.branch_if_false(lbl2, lt0);
            let neg = b.op(fp(Inst::Negate), &[scale]);
            let conv = b.op(Inst::ConvertS32ToFP32, &[input]);
            let input_scaled = b.op(fp(Inst::Multiply), &[conv, neg]);
            let c0 = b.ir.const_(0);
            let fx = b.load_input(IoVariable::FragmentCoord, c0);
            let c1 = b.ir.const_(1);
            let fy = b.load_input(IoVariable::FragmentCoord, c1);
            let arg2 = b.ir.argument(2);
            let frag_coord = b.op(Inst::ConditionalSelect, &[arg2, fy, fx]);
            let bias = b.op(fp(Inst::Modulo), &[frag_coord, neg]);
            let with_bias = b.op(fp(Inst::Add), &[input_scaled, bias]);
            let r = b.op(Inst::ConvertFP32ToS32, &[with_bias]);
            b.ret(Some(r));
            b.mark_label(lbl2);
            3
        } else {
            2
        };
        let conv = b.op(Inst::ConvertS32ToFP32, &[input]);
        let scaled2 = b.op(fp(Inst::Multiply), &[conv, scale]);
        let r = b.op(Inst::ConvertFP32ToS32, &[scaled2]);
        b.ret(Some(r));
        b.finish("TexelFetchScale", in_args)
    }

    fn texture_size_unscale(&self, ir: &mut Ir) -> R<Function> {
        let mut b = B::new(ir);
        let input = b.ir.argument(0);
        let sampler_index = b.ir.argument(1);
        let index = self.scale_index(&mut b, sampler_index);
        let raw = b.load_cb(index, FIELD_RENDER_SCALE);
        let scale = b.op(fp(Inst::Absolute), &[raw]);
        let one = b.ir.const_f(1.0);
        let scale_is_one = b.op(fp(Inst::CompareEqual), &[scale, one]);
        let lbl = b.ir.label();
        b.branch_if_false(lbl, scale_is_one);
        b.ret(Some(input));
        b.mark_label(lbl);
        let conv = b.op(Inst::ConvertS32ToFP32, &[input]);
        let unscaled = b.op(fp(Inst::Divide), &[conv, scale]);
        let r = b.op(Inst::ConvertFP32ToS32, &[unscaled]);
        b.ret(Some(r));
        b.finish("TextureSizeUnscale", 2)
    }

    fn scale_index(&self, b: &mut B, index: Opd) -> Opd {
        let one = b.ir.const_(1);
        if self.stage == ShaderStage::Vertex {
            let c0 = b.ir.const_(0);
            let cf = b.ir.const_(FIELD_FRAGMENT_RENDER_SCALE_COUNT);
            let count = b.op_kind(Inst::Load, StorageKind::ConstantBuffer, &[c0, cf]);
            let inner = b.op(Inst::Add, &[index, count]);
            b.op(Inst::Add, &[one, inner])
        } else {
            b.op(Inst::Add, &[one, index])
        }
    }
}

fn fp(i: Inst) -> Inst {
    Inst(Inst::FP32 | i.0)
}

/// A small `EmitterContext`: operations are appended in call order.
pub(crate) struct B<'a> {
    pub(crate) ir: &'a mut Ir,
    ops: Vec<NodeId>,
}

impl<'a> B<'a> {
    pub(crate) fn new(ir: &'a mut Ir) -> Self {
        B { ir, ops: vec![] }
    }

    pub(crate) fn op_kind(&mut self, inst: Inst, kind: StorageKind, src: &[Opd]) -> Opd {
        let d = self.ir.local();
        let n = self.ir.new_op(inst, kind, Some(d), src);
        self.ops.push(n);
        d
    }

    pub(crate) fn op(&mut self, inst: Inst, src: &[Opd]) -> Opd {
        self.op_kind(inst, StorageKind::None, src)
    }

    pub(crate) fn load_cb(&mut self, index: Opd, field: i32) -> Opd {
        let c0 = self.ir.const_(0);
        let cf = self.ir.const_(field);
        self.op_kind(Inst::Load, StorageKind::ConstantBuffer, &[c0, cf, index])
    }

    pub(crate) fn load_input(&mut self, io: IoVariable, elem: Opd) -> Opd {
        let c = self.ir.const_(io.0 as i32);
        self.op_kind(Inst::Load, StorageKind::Input, &[c, elem])
    }

    pub(crate) fn branch_if_false(&mut self, label: Opd, cond: Opd) {
        let n = self
            .ir
            .new_op(Inst::BranchIfFalse, StorageKind::None, Some(label), &[cond]);
        self.ops.push(n);
    }

    pub(crate) fn mark_label(&mut self, label: Opd) {
        let n = self
            .ir
            .new_op(Inst::MarkLabel, StorageKind::None, Some(label), &[]);
        self.ops.push(n);
    }

    pub(crate) fn ret(&mut self, v: Option<Opd>) {
        let src: Vec<Opd> = v.into_iter().collect();
        let n = self.ir.new_op(Inst::Return, StorageKind::None, None, &src);
        self.ops.push(n);
    }

    pub(crate) fn finish(self, name: &str, in_args: i32) -> R<Function> {
        self.finish_with(name, true, in_args, 0)
    }

    pub(crate) fn finish_with(
        self,
        name: &str,
        returns_value: bool,
        in_args: i32,
        out_args: i32,
    ) -> R<Function> {
        let cfg = Cfg::create(self.ir, &self.ops)?;
        Ok(Function {
            blocks: cfg.blocks,
            name: name.to_string(),
            returns_value,
            in_arguments: in_args,
            out_arguments: out_args,
        })
    }

    /// An operation with any destination (`Add(inst, storageKind, dest, sources)`).
    pub(crate) fn raw(
        &mut self,
        inst: Inst,
        kind: StorageKind,
        dest: Option<Opd>,
        src: &[Opd],
    ) -> NodeId {
        let n = self.ir.new_op(inst, kind, dest, src);
        self.ops.push(n);
        n
    }

    pub(crate) fn branch_if_true(&mut self, label: Opd, cond: Opd) {
        let n = self
            .ir
            .new_op(Inst::BranchIfTrue, StorageKind::None, Some(label), &[cond]);
        self.ops.push(n);
    }

    pub(crate) fn push(&mut self, n: NodeId) {
        self.ops.push(n);
    }

    /// The operations appended so far.
    pub(crate) fn take(self) -> Vec<NodeId> {
        self.ops
    }
}
