//! Intermediate representation (`IntermediateRepresentation/`): operands, operations and phi nodes in an arena.
//!
//! Identity matters in the C# (`Operand` objects, `AsgOp`, `UseOps`), so every operand is a handle into [`Ir::operands`].

pub mod dotnet_set;
#[rustfmt::skip]
pub mod inst;
pub mod agg;
pub mod block;
pub mod ops;
pub mod text;

pub use block::Block;
pub use dotnet_set::DotnetSet;
pub use inst::{Inst, IoVariable, StorageKind};
pub use ops::*;
