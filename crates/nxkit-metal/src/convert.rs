//! The Metal value of a plain type that another crate owns (`nxkit-pipestate`).

/// A plain value that has a Metal value.
pub(crate) trait Mtl {
    /// The Metal type.
    type Out;
    /// The Metal value.
    fn to_mtl(self) -> Self::Out;
}
