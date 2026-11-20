//! The structured AST (`AstNode`, `AstBlock`, `AstOperation`, `AstOperand`, `AstAssignment`, `AstComment`): an arena with
//! parent links and intrusive doubly linked child lists (the `LinkedList` of `AstBlock`).
//!
//! Use and def sets of operands are not kept: only the expression propagation of the optimiser reads them and it is skipped
//! in debug mode, which the translator always runs in.

use crate::ir::agg::AggType;
use crate::ir::{Inst, OperandType, StorageKind};
use crate::types::SamplerType;

/// Handle of an AST node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct A(pub u32);

/// `AstBlockType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockType {
    DoWhile,
    If,
    Else,
    ElseIf,
    Main,
    While,
}

/// Texture fields of an `AstTextureOperation`.
#[derive(Clone, Debug)]
pub struct AstTex {
    pub ty: SamplerType,
    pub format: usize,
    pub flags: u32,
    pub set: i32,
    pub binding: i32,
    pub sampler_set: i32,
    pub sampler_binding: i32,
}

impl AstTex {
    /// `IsSeparate`.
    pub fn is_separate(&self) -> bool {
        self.sampler_binding >= 0
    }
}

/// `AstBlock`.
#[derive(Clone, Debug)]
pub struct BlockData {
    pub ty: BlockType,
    pub cond: Option<A>,
    pub first: Option<A>,
    pub last: Option<A>,
    pub count: usize,
}

/// `AstOperation` and `AstTextureOperation`.
#[derive(Clone, Debug)]
pub struct OpData {
    pub inst: Inst,
    pub storage_kind: StorageKind,
    pub force_precise: bool,
    pub index: i32,
    pub sources: Vec<A>,
    pub tex: Option<AstTex>,
}

/// The kind of a node.
#[derive(Clone, Debug)]
pub enum Kind {
    Block(BlockData),
    Operand {
        ty: OperandType,
        var_type: AggType,
        value: i32,
    },
    Operation(OpData),
    Assign {
        dest: A,
        src: A,
    },
    Comment(String),
}

/// A node with its tree links.
#[derive(Clone, Debug)]
pub struct Node {
    pub parent: Option<A>,
    pub prev: Option<A>,
    pub next: Option<A>,
    pub kind: Kind,
}

/// The arena.
#[derive(Default, Debug)]
pub struct Ast {
    pub nodes: Vec<Node>,
}

impl Ast {
    fn push(&mut self, kind: Kind) -> A {
        self.nodes.push(Node {
            parent: None,
            prev: None,
            next: None,
            kind,
        });
        A(self.nodes.len() as u32 - 1)
    }

    pub fn node(&self, a: A) -> &Node {
        &self.nodes[a.0 as usize]
    }

    fn node_mut(&mut self, a: A) -> &mut Node {
        &mut self.nodes[a.0 as usize]
    }

    pub fn kind(&self, a: A) -> &Kind {
        &self.node(a).kind
    }

    /// `new AstBlock(type, condition)`.
    pub fn new_block(&mut self, ty: BlockType, cond: Option<A>) -> A {
        self.push(Kind::Block(BlockData {
            ty,
            cond,
            first: None,
            last: None,
            count: 0,
        }))
    }

    /// `new AstOperand(type, value)` (a local starts as S32).
    pub fn new_operand(&mut self, ty: OperandType, value: i32) -> A {
        self.push(Kind::Operand {
            ty,
            var_type: AggType::S32,
            value,
        })
    }

    pub fn new_const(&mut self, v: i32) -> A {
        self.new_operand(OperandType::Constant, v)
    }

    /// `AstHelper.Local(type)`.
    pub fn new_local(&mut self, ty: AggType) -> A {
        self.push(Kind::Operand {
            ty: OperandType::LocalVariable,
            var_type: ty,
            value: 0,
        })
    }

    /// `new AstAssignment(destination, source)`.
    pub fn new_assign(&mut self, dest: A, src: A) -> A {
        self.push(Kind::Assign { dest, src })
    }

    pub fn new_comment(&mut self, text: &str) -> A {
        self.push(Kind::Comment(text.to_string()))
    }

    /// `new AstOperation(inst, storageKind, forcePrecise, index, sources, ...)`.
    pub fn new_op(
        &mut self,
        inst: Inst,
        storage_kind: StorageKind,
        force_precise: bool,
        index: i32,
        sources: Vec<A>,
    ) -> A {
        self.push(Kind::Operation(OpData {
            inst,
            storage_kind,
            force_precise,
            index,
            sources,
            tex: None,
        }))
    }

    /// `new AstOperation(inst, params sources)`.
    pub fn new_simple_op(&mut self, inst: Inst, sources: Vec<A>) -> A {
        self.new_op(inst, StorageKind::None, false, 0, sources)
    }

    pub fn new_tex_op(&mut self, inst: Inst, tex: AstTex, index: i32, sources: Vec<A>) -> A {
        self.push(Kind::Operation(OpData {
            inst,
            storage_kind: StorageKind::None,
            force_precise: false,
            index,
            sources,
            tex: Some(tex),
        }))
    }

    // ---- accessors (a node of the wrong kind is an internal error: callers check the kind first) ----

    pub fn block(&self, a: A) -> Option<&BlockData> {
        match &self.node(a).kind {
            Kind::Block(b) => Some(b),
            _ => None,
        }
    }

    fn block_mut(&mut self, a: A) -> &mut BlockData {
        match &mut self.node_mut(a).kind {
            Kind::Block(b) => b,
            _ => unreachable_block(),
        }
    }

    pub fn op(&self, a: A) -> Option<&OpData> {
        match &self.node(a).kind {
            Kind::Operation(o) => Some(o),
            _ => None,
        }
    }

    pub fn is_block(&self, a: A) -> bool {
        matches!(self.node(a).kind, Kind::Block(_))
    }

    pub fn block_type(&self, a: A) -> Option<BlockType> {
        self.block(a).map(|b| b.ty)
    }

    pub fn cond(&self, a: A) -> Option<A> {
        self.block(a).and_then(|b| b.cond)
    }

    pub fn parent(&self, a: A) -> Option<A> {
        self.node(a).parent
    }

    pub fn first(&self, b: A) -> Option<A> {
        self.block(b).and_then(|b| b.first)
    }

    pub fn last(&self, b: A) -> Option<A> {
        self.block(b).and_then(|b| b.last)
    }

    pub fn count(&self, b: A) -> usize {
        self.block(b).map_or(0, |b| b.count)
    }

    /// `AstHelper.Next`.
    pub fn next(&self, a: A) -> Option<A> {
        self.node(a).next
    }

    /// `AstHelper.Previous`.
    pub fn prev(&self, a: A) -> Option<A> {
        self.node(a).prev
    }

    /// The children of a block in order.
    pub fn children(&self, b: A) -> Vec<A> {
        let mut v = Vec::new();
        let mut n = self.first(b);
        while let Some(x) = n {
            v.push(x);
            n = self.next(x);
        }
        v
    }

    pub fn set_condition(&mut self, b: A, cond: Option<A>) {
        self.block_mut(b).cond = cond;
    }

    pub fn set_type(&mut self, b: A, ty: BlockType) {
        self.block_mut(b).ty = ty;
    }

    // ---- list operations (`Add`, `AddFirst`, `AddBefore`, `AddAfter`, `Remove`) ----

    fn attach(&mut self, block: A, node: A) -> Result<(), String> {
        if self.node(node).parent.is_some() {
            return Err("Node already belongs to a block.".into());
        }
        self.node_mut(node).parent = Some(block);
        self.block_mut(block).count += 1;
        Ok(())
    }

    /// `Add` (append).
    pub fn add(&mut self, block: A, node: A) -> Result<(), String> {
        self.attach(block, node)?;
        let last = self.block(block).and_then(|b| b.last);
        self.node_mut(node).prev = last;
        self.node_mut(node).next = None;
        match last {
            Some(l) => self.node_mut(l).next = Some(node),
            None => self.block_mut(block).first = Some(node),
        }
        self.block_mut(block).last = Some(node);
        Ok(())
    }

    pub fn add_first(&mut self, block: A, node: A) -> Result<(), String> {
        self.attach(block, node)?;
        let first = self.block(block).and_then(|b| b.first);
        self.node_mut(node).next = first;
        self.node_mut(node).prev = None;
        match first {
            Some(f) => self.node_mut(f).prev = Some(node),
            None => self.block_mut(block).last = Some(node),
        }
        self.block_mut(block).first = Some(node);
        Ok(())
    }

    pub fn add_before(&mut self, block: A, next: A, node: A) -> Result<(), String> {
        self.attach(block, node)?;
        let prev = self.node(next).prev;
        self.node_mut(node).prev = prev;
        self.node_mut(node).next = Some(next);
        self.node_mut(next).prev = Some(node);
        match prev {
            Some(p) => self.node_mut(p).next = Some(node),
            None => self.block_mut(block).first = Some(node),
        }
        Ok(())
    }

    pub fn add_after(&mut self, block: A, prev: A, node: A) -> Result<(), String> {
        self.attach(block, node)?;
        let next = self.node(prev).next;
        self.node_mut(node).next = next;
        self.node_mut(node).prev = Some(prev);
        self.node_mut(prev).next = Some(node);
        match next {
            Some(n) => self.node_mut(n).prev = Some(node),
            None => self.block_mut(block).last = Some(node),
        }
        Ok(())
    }

    /// `Remove`.
    pub fn remove(&mut self, block: A, node: A) {
        let (prev, next) = (self.node(node).prev, self.node(node).next);
        match prev {
            Some(p) => self.node_mut(p).next = next,
            None => self.block_mut(block).first = next,
        }
        match next {
            Some(n) => self.node_mut(n).prev = prev,
            None => self.block_mut(block).last = prev,
        }
        let n = self.node_mut(node);
        n.parent = None;
        n.prev = None;
        n.next = None;
        self.block_mut(block).count -= 1;
    }
}

fn unreachable_block() -> ! {
    // a block operation on a node that is not a block: an internal invariant violation (the C# would fail the cast)
    #[allow(clippy::panic)]
    {
        panic!("AST node is not a block")
    }
}
