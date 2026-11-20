//! `EmitterContext`: the operation list being built for one function, with the labels of its blocks.

use crate::accessor::Accessor;
use crate::ir::inst::{Inst, StorageKind};
use crate::ir::ops::{Ir, NodeId, Opd};
use crate::maxwell::decoder::{Block as DecodedBlock, DecodedFunction};
use crate::maxwell::table::InstOp;
use crate::translation::context::TranslatorContext;
use crate::translation::resources::ResourceManager;
use std::collections::HashMap;

/// `Instruction.FP32` and `Instruction.FP64` as instruction values (the default `fpType` is FP32).
pub const FP32: Inst = Inst(Inst::FP32);
pub const FP64: Inst = Inst(Inst::FP64);

impl std::ops::BitOr for Inst {
    type Output = Inst;
    fn bitor(self, o: Inst) -> Inst {
        Inst(self.0 | o.0)
    }
}

#[derive(Clone, Copy)]
struct BrxTarget {
    selector: Opd,
    expected_value: i32,
    next_target_address: u64,
}

struct BlockLabel {
    label: Opd,
    brx: Option<BrxTarget>,
}

/// The state of `EmitterContext`: `ir` is the arena all operands and operations live in.
pub struct EmitterContext<'a, 'b> {
    pub ir: Ir,
    pub tc: &'a TranslatorContext,
    pub acc: &'a mut Accessor<'b>,
    pub rm: &'a mut ResourceManager,
    pub vertex_as_compute: bool,
    pub is_non_main: bool,
    pub curr_block: Option<&'a DecodedBlock>,
    pub curr_function: Option<&'a DecodedFunction>,
    pub curr_op: InstOp,
    /// Test mode: an emitter error is recorded in `errors` and emission goes on (per-instruction comparison).
    pub lenient: bool,
    pub errors: Vec<(String, String)>,
    /// Addresses of the functions in id order (`DecodedProgram.GetFunctionByAddress(..).Id`).
    pub function_addresses: Vec<u64>,
    /// The functions `FunctionMatch` replaced: the fragment shader interlock built-ins and the dropped helpers.
    pub builtin_functions: Vec<(u64, crate::translation::fsi_match::FunctionType)>,
    ops: Vec<NodeId>,
    tag_locals: HashMap<u32, Opd>,
    labels: HashMap<u64, BlockLabel>,
}

impl<'a, 'b> EmitterContext<'a, 'b> {
    pub fn new(
        ir: Ir,
        tc: &'a TranslatorContext,
        acc: &'a mut Accessor<'b>,
        rm: &'a mut ResourceManager,
        vertex_as_compute: bool,
        is_non_main: bool,
    ) -> EmitterContext<'a, 'b> {
        EmitterContext {
            ir,
            tc,
            acc,
            rm,
            vertex_as_compute,
            is_non_main,
            curr_block: None,
            curr_function: None,
            curr_op: InstOp::none(),
            lenient: false,
            errors: Vec::new(),
            function_addresses: Vec::new(),
            builtin_functions: Vec::new(),
            ops: Vec::new(),
            tag_locals: HashMap::new(),
            labels: HashMap::new(),
        }
    }

    /// The local of a SSY/PBK consumer (`PushOpInfo.Consumers`), created on first use, the decoder only keeps its tag.
    pub fn tag_local(&mut self, tag: u32) -> Opd {
        if let Some(o) = self.tag_locals.get(&tag) {
            return *o;
        }
        let o = self.ir.local();
        self.tag_locals.insert(tag, o);
        o
    }

    /// `GetOperations` plus the arena.
    pub fn finish(self) -> (Ir, Vec<NodeId>) {
        (self.ir, self.ops)
    }

    pub fn operations_count(&self) -> usize {
        self.ops.len()
    }

    pub fn operation(&self, index: usize) -> NodeId {
        self.ops[index]
    }

    pub fn insert_operation(&mut self, index: usize, n: NodeId) {
        self.ops.insert(index, n);
    }

    /// `Add(Operation)`.
    pub fn add_node(&mut self, n: NodeId) {
        self.ops.push(n);
    }

    /// `Add(inst, storageKind, dest, sources)`.
    pub fn add_op(
        &mut self,
        inst: Inst,
        kind: StorageKind,
        dest: Option<Opd>,
        sources: &[Opd],
    ) -> NodeId {
        let n = self.ir.new_op(inst, kind, dest, sources);
        self.ops.push(n);
        n
    }

    /// `Add(inst, dest, sources)` without a storage kind.
    pub fn add(&mut self, inst: Inst, dest: Option<Opd>, sources: &[Opd]) -> Option<Opd> {
        self.add_op(inst, StorageKind::None, dest, sources);
        dest
    }

    /// `Add(inst, (d0, d1), sources)`: an operation with two destinations (index 0).
    pub fn add2(&mut self, inst: Inst, d0: Opd, d1: Opd, sources: &[Opd]) -> (Opd, Opd) {
        let n = self
            .ir
            .new_op_multi(inst, 0, &[Some(d0), Some(d1)], sources);
        self.ops.push(n);
        (d0, d1)
    }

    pub fn comment(&mut self, text: &str) {
        let n = self.ir.new_comment(text);
        self.ops.push(n);
    }

    pub fn mark_label(&mut self, label: Opd) {
        self.add_op(Inst::MarkLabel, StorageKind::None, Some(label), &[]);
    }

    fn ensure_block_label(&mut self, address: u64) -> &mut BlockLabel {
        let ir = &mut self.ir;
        self.labels.entry(address).or_insert_with(|| BlockLabel {
            label: ir.label(),
            brx: None,
        })
    }

    pub fn get_label(&mut self, address: u64) -> Opd {
        self.ensure_block_label(address).label
    }

    pub fn set_brx_target(
        &mut self,
        address: u64,
        selector: Opd,
        target_value: i32,
        next_target_address: u64,
    ) {
        self.ensure_block_label(address).brx = Some(BrxTarget {
            selector,
            expected_value: target_value,
            next_target_address,
        });
    }

    /// `EnterBlock`.
    pub fn enter_block(&mut self, address: u64) {
        let bl = self.ensure_block_label(address);
        let label = bl.label;
        let brx = bl.brx;
        self.mark_label(label);
        if let Some(b) = brx {
            let next = self.get_label(b.next_target_address);
            let c = self.ir.const_(b.expected_value);
            let eq = self.i_compare_equal(b.selector, c);
            self.branch_if_false(next, eq);
        }
    }

    /// `Copy(d, a)` (the C# overload with a destination.`copy(a)` makes a new local): nothing for a constant destination.
    pub fn copy_to(&mut self, d: Opd, a: Opd) {
        if self.ir.ty(d) == crate::ir::ops::OperandType::Constant {
            return;
        }
        self.add_op(Inst::Copy, StorageKind::None, Some(d), &[a]);
    }
}

impl EmitterContext<'_, '_> {
    /// Index of `curr_block` in the current function.
    pub fn curr_block_index(&self) -> Result<usize, String> {
        let (Some(b), Some(f)) = (self.curr_block, self.curr_function) else {
            return Err("no current block".into());
        };
        f.blocks
            .iter()
            .position(|x| std::ptr::eq(x, b))
            .ok_or_else(|| "current block not in function".to_string())
    }

    /// The address of block `index` of the current function (`Block.Successors[i].Address`).
    pub fn block_address(&self, index: usize) -> u64 {
        self.curr_function.map_or(0, |f| f.blocks[index].address)
    }
}
