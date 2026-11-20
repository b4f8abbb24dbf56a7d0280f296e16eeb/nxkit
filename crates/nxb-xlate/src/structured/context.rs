//! `StructuredProgramContext`: turns the basic blocks of a function into nested AST blocks (do-while loops, ifs) and
//! records the branches it cannot nest as gotos for `goto::eliminate`.

use super::ast::*;
use super::{GotoStatement, IoDef, StructFunction, StructInfo, StructResources};
use crate::ir::agg::AggType;
use crate::ir::{Block, FALSE, Inst, Ir, NodeId, Opd, OperandType, StorageKind, TRUE};
use crate::props::Definitions;
use std::collections::{HashMap, HashSet};

pub struct Context<'a> {
    pub ast: Ast,
    pub info: StructInfo,
    pub defs: &'a Definitions,
    pub resources: &'a mut dyn StructResources,
    pub debug_mode: bool,
    loop_tails: HashSet<usize>,
    block_stack: Vec<(A, usize, usize)>,
    locals_map: HashMap<Opd, A>,
    goto_temp_asgs: HashMap<usize, A>,
    pub gotos: Vec<GotoStatement>,
    curr_block: A,
    curr_end_index: usize,
    loop_end_index: usize,
    /// The function being built.
    pub cur: StructFunction,
}

impl<'a> Context<'a> {
    pub fn new(
        defs: &'a Definitions,
        resources: &'a mut dyn StructResources,
        passthrough_attributes: i32,
        debug_mode: bool,
    ) -> Context<'a> {
        let mut ast = Ast::default();
        let main = ast.new_block(BlockType::Main, None);
        let mut info = StructInfo::default();
        if defs.gp_passthrough {
            let mut pa = passthrough_attributes;
            while pa != 0 {
                let index = pa.trailing_zeros() as i32;
                info.add_io(IoDef {
                    storage_kind: StorageKind::Input,
                    io_variable: crate::ir::IoVariable::UserDefined,
                    location: index,
                    component: 0,
                });
                pa &= !(1 << index);
            }
            for v in [
                crate::ir::IoVariable::Position,
                crate::ir::IoVariable::PointSize,
                crate::ir::IoVariable::ClipDistance,
            ] {
                info.add_io(IoDef {
                    storage_kind: StorageKind::Input,
                    io_variable: v,
                    location: 0,
                    component: 0,
                });
            }
        }
        Context {
            ast,
            info,
            defs,
            resources,
            debug_mode,
            loop_tails: HashSet::new(),
            block_stack: Vec::new(),
            locals_map: HashMap::new(),
            goto_temp_asgs: HashMap::new(),
            gotos: Vec::new(),
            curr_block: main,
            curr_end_index: 0,
            loop_end_index: 0,
            cur: StructFunction {
                main_block: main,
                name: String::new(),
                return_type: AggType::VOID,
                in_arguments: vec![],
                out_arguments: vec![],
                locals: vec![],
            },
        }
    }

    pub fn enter_function(
        &mut self,
        blocks_count: usize,
        name: &str,
        return_type: AggType,
        in_arguments: Vec<AggType>,
        out_arguments: Vec<AggType>,
    ) {
        self.loop_tails.clear();
        self.block_stack.clear();
        self.locals_map.clear();
        self.goto_temp_asgs.clear();
        self.gotos.clear();
        let main = self.ast.new_block(BlockType::Main, None);
        self.curr_block = main;
        self.curr_end_index = blocks_count;
        self.loop_end_index = blocks_count;
        self.cur = StructFunction {
            main_block: main,
            name: name.to_string(),
            return_type,
            in_arguments,
            out_arguments,
            locals: vec![],
        };
    }

    pub fn leave_function(&mut self) {
        let f = std::mem::replace(
            &mut self.cur,
            StructFunction {
                main_block: self.curr_block,
                name: String::new(),
                return_type: AggType::VOID,
                in_arguments: vec![],
                out_arguments: vec![],
                locals: vec![],
            },
        );
        self.info.functions.push(f);
    }

    pub fn enter_block(&mut self, ir: &Ir, blocks: &[Block], index: usize) -> Result<(), String> {
        while self.curr_end_index == index {
            let (b, c, l) = self.block_stack.pop().ok_or("block stack underflow")?;
            self.curr_block = b;
            self.curr_end_index = c;
            self.loop_end_index = l;
        }
        if let Some(&asg) = self.goto_temp_asgs.get(&index) {
            self.add_goto_temp_reset(index, asg)?;
        }
        self.look_for_do_while_statements(ir, blocks, index)
    }

    pub fn leave_block(
        &mut self,
        ir: &Ir,
        blocks: &[Block],
        index: usize,
        branch_op: NodeId,
    ) -> Result<(), String> {
        self.look_for_if_statements(ir, blocks, index, branch_op)
    }

    fn look_for_do_while_statements(
        &mut self,
        ir: &Ir,
        blocks: &[Block],
        index: usize,
    ) -> Result<(), String> {
        let mut done = false;
        let mut preds = blocks[index].preds.clone();
        // OrderByDescending is a stable sort
        preds.sort_by(|a, b| b.cmp(a));
        for pred in preds {
            if pred < index {
                break;
            }
            if pred < self.curr_end_index && !done {
                let last = *blocks[pred]
                    .ops
                    .last()
                    .ok_or("loop tail without operations")?;
                self.new_block_op(ir, BlockType::DoWhile, last, pred + 1)?;
                self.loop_tails.insert(pred);
                done = true;
            } else {
                let asg = self.get_goto_temp_asg(index);
                self.add_goto_temp_reset(index, asg)?;
                break;
            }
        }
        Ok(())
    }

    fn look_for_if_statements(
        &mut self,
        ir: &Ir,
        blocks: &[Block],
        index: usize,
        branch_op: NodeId,
    ) -> Result<(), String> {
        let Some(branch) = blocks[index].branch else {
            return Ok(());
        };
        let in_range = branch < self.curr_end_index
            || (branch == self.curr_end_index && branch < self.loop_end_index);
        let is_loop = branch <= index;
        if in_range && !is_loop {
            self.new_block_op(ir, BlockType::If, branch_op, branch)?;
        } else if !self.loop_tails.contains(&index) {
            let goto_temp_asg = self.get_goto_temp_asg(branch);
            let cond = self.get_branch_cond(ir, BlockType::DoWhile, branch_op)?;
            let dest = match self.ast.kind(goto_temp_asg) {
                Kind::Assign { dest, .. } => *dest,
                _ => return Err("goto temp is not an assignment".into()),
            };
            let asg = self.ast.new_assign(dest, cond);
            self.add_node(asg)?;
            let inst = ir.op(branch_op).inst;
            let b = self.ast.new_simple_op(inst, vec![]);
            self.add_node(b)?;
            self.gotos.push(GotoStatement {
                goto: b,
                label: goto_temp_asg,
                is_loop,
            });
        }
        Ok(())
    }

    fn get_goto_temp_asg(&mut self, index: usize) -> A {
        if let Some(&a) = self.goto_temp_asgs.get(&index) {
            return a;
        }
        let temp = self.new_temp(AggType::BOOL);
        let f = self.ast.new_const(FALSE);
        let asg = self.ast.new_assign(temp, f);
        self.goto_temp_asgs.insert(index, asg);
        asg
    }

    fn add_goto_temp_reset(&mut self, block_index: usize, asg: A) -> Result<(), String> {
        if self.ast.parent(asg).is_some() {
            return Ok(());
        }
        self.add_node(asg)?;
        if block_index != 0 {
            let dest = match self.ast.kind(asg) {
                Kind::Assign { dest, .. } => *dest,
                _ => return Err("goto temp is not an assignment".into()),
            };
            let f = self.ast.new_const(FALSE);
            let reset = self.ast.new_assign(dest, f);
            let main = self.cur.main_block;
            self.ast.add_first(main, reset)?;
        }
        Ok(())
    }

    fn new_block_op(
        &mut self,
        ir: &Ir,
        ty: BlockType,
        branch_op: NodeId,
        end_index: usize,
    ) -> Result<(), String> {
        let cond = self.get_branch_cond(ir, ty, branch_op)?;
        self.new_block(ty, Some(cond), end_index)
    }

    fn new_block(
        &mut self,
        ty: BlockType,
        cond: Option<A>,
        end_index: usize,
    ) -> Result<(), String> {
        let child = self.ast.new_block(ty, cond);
        self.add_node(child)?;
        self.block_stack
            .push((self.curr_block, self.curr_end_index, self.loop_end_index));
        self.curr_block = child;
        self.curr_end_index = end_index;
        if ty == BlockType::DoWhile {
            self.loop_end_index = end_index;
        }
        Ok(())
    }

    fn get_branch_cond(&mut self, ir: &Ir, ty: BlockType, branch_op: NodeId) -> Result<A, String> {
        let op = ir.op(branch_op);
        if op.inst == Inst::Branch {
            Ok(self
                .ast
                .new_const(if ty == BlockType::If { FALSE } else { TRUE }))
        } else {
            let src = op
                .sources
                .first()
                .copied()
                .flatten()
                .ok_or("branch without condition")?;
            let mut cond = self
                .get_operand(ir, Some(src))
                .ok_or("no condition operand")?;
            let inv = if ty == BlockType::If {
                Inst::BranchIfTrue
            } else {
                Inst::BranchIfFalse
            };
            if op.inst == inv {
                cond = self.ast.new_simple_op(Inst::LogicalNot, vec![cond]);
            }
            Ok(cond)
        }
    }

    pub fn add_node(&mut self, node: A) -> Result<(), String> {
        let b = self.curr_block;
        self.ast.add(b, node)
    }

    pub fn new_temp(&mut self, ty: AggType) -> A {
        let t = self.ast.new_local(ty);
        self.cur.locals.push(t);
        t
    }

    pub fn get_operand_or_cb_load(&mut self, ir: &Ir, operand: Opd) -> Result<A, String> {
        if ir.ty(operand) == OperandType::ConstantBuffer {
            let slot = ir.cbuf_slot(operand);
            let offset = ir.cbuf_offset(operand);
            let binding = self.resources.constant_buffer_binding(slot)?;
            let vec_index = offset >> 2;
            let elem_index = offset & 3;
            self.resources.set_used_constant_buffer_binding(binding);
            let sources = vec![
                self.ast.new_const(binding),
                self.ast.new_const(0),
                self.ast.new_const(vec_index),
                self.ast.new_const(elem_index),
            ];
            return Ok(self
                .ast
                .new_op(Inst::Load, StorageKind::ConstantBuffer, false, 0, sources));
        }
        self.get_operand(ir, Some(operand))
            .ok_or_else(|| "null operand".to_string())
    }

    /// `GetOperand`: null stays null, locals map to one AST operand each.
    pub fn get_operand(&mut self, ir: &Ir, operand: Option<Opd>) -> Option<A> {
        let operand = operand?;
        let d = ir.data(operand);
        if d.ty != OperandType::LocalVariable {
            return Some(self.ast.new_operand(d.ty, d.value));
        }
        if let Some(&a) = self.locals_map.get(&operand) {
            return Some(a);
        }
        let a = self.ast.new_operand(d.ty, d.value);
        self.locals_map.insert(operand, a);
        self.cur.locals.push(a);
        Some(a)
    }
}
