//! The emitter core: values, operands, hoisted temporaries and statement output.
//!
//! A structured expression becomes an MSL expression string with its type. Pure operations stay inline. An operation
//! with an effect, or one that a template uses twice, is hoisted to a temporary declared before the statement.

use super::args::Res;
use super::io::Plan;
use super::types::{cast, literal, msl_type, scalar};
use crate::ir::{Inst, OperandType};
use crate::props::Props;
use crate::structured::StructInfo;
use crate::structured::ast::{A, Kind, OpData};
use crate::structured::types::AggType;
use std::collections::{BTreeMap, BTreeSet};

/// The result type of the emitter functions.
pub type R<T> = Result<T, String>;

/// The kind of the shader being emitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shader {
    /// Vertex shader.
    Vertex,
    /// Fragment shader.
    Fragment,
    /// Compute shader.
    Compute,
}

/// An MSL expression and its type.
#[derive(Clone, Debug)]
pub struct Val {
    /// The expression.
    pub s: String,
    /// The type.
    pub ty: AggType,
}

impl Val {
    /// A value.
    pub fn new(s: impl Into<String>, ty: AggType) -> Val {
        Val { s: s.into(), ty }
    }
}

/// What the emitter was asked to do besides the plain translation.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The vertex shader is the visibility counter: lane reads are 0 and ballots are one lane.
    pub visibility_counter: bool,
    /// Pixel shaders: write `main0_d` for a shader that writes exactly the colours 0 and 1 (the profile's dual source
    /// list says so).
    pub dual_source: bool,
    /// Pixel shaders: write the `_bf` library of a shader without a rewritable sample too (the profile's fetch list
    /// says so).
    pub fetch_outputs_only: bool,
    /// Declare every local on a line of its own, with its zero value. The text transform of the lean geometry kernel
    /// reads the declarations line by line (see `geom_lean`).
    pub plain_decls: bool,
}

/// The reads of the driver constant banks.
#[derive(Debug, Default)]
pub struct DrvReads {
    /// Bank to the vec4 indices read at constant indices.
    pub words: BTreeMap<u32, BTreeSet<u32>>,
    /// Banks read at a run time index.
    pub dynamic: BTreeSet<u32>,
}

/// Names of the extra parameters every helper function takes and the main function passes on.
#[derive(Clone, Debug, Default)]
pub struct Shared {
    /// `(declaration, argument)` pairs.
    pub params: Vec<(String, String)>,
    /// The shared memories live in the device buffer of the threadgroup spill (see `spill`).
    pub spill: bool,
    /// Blocks of the main function the emitter leaves out (see `first_store`).
    pub skip: BTreeSet<A>,
    /// The loads that are lanes of a wide load (see `wide`).
    pub wide: std::collections::BTreeMap<A, super::wide::Lane>,
}

/// The emitter state.
pub struct Em<'a> {
    pub(super) info: &'a StructInfo,
    pub(super) props: &'a Props,
    pub(super) res: &'a Res,
    pub(super) opt: &'a Options,
    pub(super) shared: &'a Shared,
    pub(super) plan: &'a Plan,
    pub(super) shader: Shader,
    pub(super) out: String,
    pub(super) ind: usize,
    pub(super) pre: Vec<String>,
    pub(super) tmp: u32,
    pub(super) func: usize,
    pub(super) cur: Option<A>,
    pub(super) may_return: bool,
    /// The function being written calls the HMMA helper and declares its matrices (see `hmma`).
    pub(super) hmma_caller: bool,
    /// The loops being emitted. A loop that evaluates its condition with statements has a flag that its breaks set.
    pub(super) loops: Vec<Option<String>>,
    /// The driver bank reads of the program.
    pub drv: DrvReads,
    /// The notes of the emitter.
    pub notes: Vec<String>,
    /// The prelude helper functions the body calls (see `prelude`).
    pub helpers: BTreeSet<&'static str>,
    /// The lines a vertex entry writes at each return of the main function instead of the default tail.
    pub exit: Option<Vec<String>>,
    /// The stores of integer values to user outputs of a vertex shader: function, location and component.
    pub layer_src: Vec<(usize, i32, i32)>,
    /// The conversions `get` made while the vertex inputs are probed (see `variant::int_inputs`).
    pub probe: Option<Vec<super::variant::int_inputs::Use>>,
    /// The type of the last operand `get` converted, before the conversion.
    pub(super) last_src: Option<AggType>,
    /// The fetch variant is being written: the rewritten samples so far.
    pub(super) fetch: Option<super::variant::fetch::State>,
}

impl<'a> Em<'a> {
    /// A fresh emitter.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        info: &'a StructInfo,
        props: &'a Props,
        res: &'a Res,
        opt: &'a Options,
        shared: &'a Shared,
        plan: &'a Plan,
        shader: Shader,
    ) -> Em<'a> {
        Em {
            info,
            props,
            res,
            opt,
            shared,
            plan,
            shader,
            out: String::new(),
            ind: 1,
            pre: Vec::new(),
            tmp: 0,
            func: 0,
            cur: None,
            may_return: false,
            hmma_caller: false,
            loops: Vec::new(),
            drv: DrvReads::default(),
            notes: Vec::new(),
            helpers: BTreeSet::new(),
            exit: None,
            layer_src: Vec::new(),
            probe: None,
            last_src: None,
            fetch: None,
        }
    }

    /// The text emitted so far.
    pub fn take(&mut self) -> String {
        std::mem::take(&mut self.out)
    }

    /// One line at the current indentation, after the hoisted lines of the statement.
    pub(super) fn line(&mut self, s: &str) {
        self.flush();
        self.raw(s);
    }

    pub(super) fn raw(&mut self, s: &str) {
        for _ in 0..self.ind {
            self.out.push_str("    ");
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    /// Write the hoisted lines.
    pub(super) fn flush(&mut self) {
        for p in std::mem::take(&mut self.pre) {
            self.raw(&p);
        }
    }

    /// Hoist a value to a temporary.
    pub(super) fn hoist(&mut self, v: Val) -> R<Val> {
        let n = self.tmp;
        self.tmp += 1;
        let t = msl_type(v.ty)?;
        self.pre.push(format!("{t} t{n} = {};", v.s));
        Ok(Val::new(format!("t{n}"), v.ty))
    }

    /// The value as a name or literal, hoisted when it is an expression.
    pub(super) fn once(&mut self, v: Val) -> R<Val> {
        if v.s
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        {
            Ok(v)
        } else {
            self.hoist(v)
        }
    }

    /// The node `a` as a value of type `ty`. Constants are literals of that type.
    pub(super) fn get(&mut self, ty: AggType, a: A) -> R<Val> {
        if let Kind::Operand { ty: ot, value, .. } = self.info.ast.kind(a) {
            match ot {
                OperandType::Constant => {
                    self.last_src = None;
                    return Ok(Val::new(literal(ty, *value), ty));
                }
                OperandType::Undefined => {
                    self.last_src = None;
                    return Ok(Val::new(literal(ty, 0), ty));
                }
                _ => {}
            }
        }
        let v = self.eval(a)?;
        self.last_src = Some(v.ty);
        if let Some(p) = &mut self.probe {
            p.push((a, ty, v.ty));
        }
        Ok(self.as_type(v, ty))
    }

    /// A value converted by bit cast to another type.
    pub(super) fn as_type(&self, v: Val, ty: AggType) -> Val {
        let vec = |t: AggType| t.0 & AggType::ELEMENT_COUNT_MASK != 0;
        let s = if vec(v.ty) || vec(ty) {
            if scalar(v.ty) == scalar(ty) {
                v.s
            } else {
                match msl_type(ty) {
                    Ok(t) => format!("as_type<{t}>({})", v.s),
                    Err(_) => v.s,
                }
            }
        } else {
            cast(&v.s, v.ty, ty)
        };
        Val::new(s, ty)
    }

    /// The node `a` with its own type.
    pub(super) fn eval(&mut self, a: A) -> R<Val> {
        if let Some(v) = self.wide_lane(a)? {
            return Ok(v);
        }
        match self.info.ast.kind(a).clone() {
            Kind::Operation(o) => self
                .operation(&o)?
                .ok_or_else(|| "operation produced no value".to_string()),
            Kind::Operand {
                ty,
                var_type,
                value,
            } => match ty {
                OperandType::Argument => Ok(Val::new(format!("a{value}"), var_type)),
                OperandType::LocalVariable => Ok(Val::new(format!("v{}", a.0), var_type)),
                OperandType::Constant => Ok(Val::new(literal(AggType::S32, value), AggType::S32)),
                OperandType::Undefined => Ok(Val::new("0", AggType::S32)),
                other => Err(format!("invalid operand type {other:?}")),
            },
            _ => Err("node is neither an operation nor an operand".into()),
        }
    }

    /// The constant a node holds.
    pub(super) fn constant(&self, a: A) -> Option<i32> {
        super::usage::const_of(&self.info.ast, a)
    }

    /// The source `i` of an operation.
    pub(super) fn src(&self, o: &OpData, i: usize) -> R<A> {
        o.sources
            .get(i)
            .copied()
            .ok_or_else(|| "missing source".to_string())
    }

    /// `T tN = init;` for a statement result that has an effect and is used once.
    pub(super) fn effect(&mut self, s: String, ty: AggType) -> R<Val> {
        self.hoist(Val::new(s, ty))
    }

    /// The instruction without its flags.
    pub(super) fn inst_of(o: &OpData) -> Inst {
        Inst(o.inst.0 & Inst::MASK)
    }
}
