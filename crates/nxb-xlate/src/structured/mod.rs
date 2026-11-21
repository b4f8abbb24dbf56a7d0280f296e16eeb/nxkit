//! StructuredIr (`StructuredIr/`): the final IR as an AST of blocks, ifs and loops.

pub mod ast;
pub mod context;
pub mod goto;
pub mod optimizer;
pub mod program;
#[cfg(test)]
mod test_util;
#[cfg(test)]
mod tests;
pub mod text;
pub mod walk;

use crate::ir::agg::AggType;
use crate::ir::{IoVariable, StorageKind};
pub use ast::{A, Ast, BlockType};
pub use program::make_structured_program;

/// `StructuredFunction`.
#[derive(Clone, Debug)]
pub struct StructFunction {
    pub main_block: A,
    pub name: String,
    pub return_type: AggType,
    pub in_arguments: Vec<AggType>,
    pub out_arguments: Vec<AggType>,
    /// `Locals` in insertion order
    pub locals: Vec<A>,
}

impl StructFunction {
    /// `GetArgumentType`.
    pub fn argument_type(&self, index: usize) -> AggType {
        if index >= self.in_arguments.len() {
            self.out_arguments[index - self.in_arguments.len()]
        } else {
            self.in_arguments[index]
        }
    }
}

/// `IoDefinition`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IoDef {
    pub storage_kind: StorageKind,
    pub io_variable: IoVariable,
    pub location: i32,
    pub component: i32,
}

/// `HelperFunctionsMask` bits.
pub mod helper {
    pub const MULTIPLY_HIGH_S32: u32 = 1 << 2;
    pub const MULTIPLY_HIGH_U32: u32 = 1 << 3;
    pub const SWIZZLE_ADD: u32 = 1 << 10;
    pub const FSI: u32 = 1 << 11;
}

/// `StructuredProgramInfo` with the AST arena that owns its nodes.
#[derive(Default, Debug)]
pub struct StructInfo {
    pub ast: Ast,
    pub functions: Vec<StructFunction>,
    /// `IoDefinitions` (a `HashSet` that is only added to: insertion order)
    pub io_definitions: Vec<IoDef>,
    pub helper_functions_mask: u32,
}

impl StructInfo {
    /// `IoDefinitions.Add`.
    pub fn add_io(&mut self, d: IoDef) {
        if !self.io_definitions.contains(&d) {
            self.io_definitions.push(d);
        }
    }

    /// `IoDefinitions.Contains`.
    pub fn has_io(&self, kind: StorageKind, v: IoVariable) -> bool {
        self.io_definitions.contains(&IoDef {
            storage_kind: kind,
            io_variable: v,
            location: 0,
            component: 0,
        })
    }
}

/// `GotoStatement`.
#[derive(Clone, Copy, Debug)]
pub struct GotoStatement {
    pub goto: A,
    pub label: A,
    pub is_loop: bool,
}

/// The parts of the `ResourceManager` the structuring calls.
pub trait StructResources {
    /// `GetConstantBufferBinding(slot)`: may create the binding.
    fn constant_buffer_binding(&mut self, slot: i32) -> Result<i32, String>;
    /// `SetUsedConstantBufferBinding`.
    fn set_used_constant_buffer_binding(&mut self, binding: i32);
}
