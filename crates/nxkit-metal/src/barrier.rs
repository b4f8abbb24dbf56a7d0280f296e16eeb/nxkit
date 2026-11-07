//! Barriers between passes and dispatches, with explicit stages.
//!
//! Metal 4 does not track hazards. The frame encoder knows which pass writes a resource and which later pass reads
//! it, so it states the dependency and this module turns it into a barrier. A [`Dependency`] names the stages on
//! both sides and what has to be visible. [`Dependency::between`] derives all three from the kind of use of the
//! producer and the consumer.

use std::ops::BitOr;

use objc2::Message;
use objc2_metal::{MTL4CommandEncoder, MTL4VisibilityOptions, MTLStages};

use crate::error::{MetalError, Result};

/// A set of pipeline stages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Stages(u32);

impl Stages {
    /// Vertex shading.
    pub const VERTEX: Self = Self(1);
    /// Fragment shading, including the writes to the attachments.
    pub const FRAGMENT: Self = Self(2);
    /// Compute dispatches.
    pub const DISPATCH: Self = Self(4);
    /// Copies.
    pub const BLIT: Self = Self(8);
    /// Every stage.
    pub const ALL: Self = Self(15);

    fn to_mtl(self) -> MTLStages {
        let mut s = MTLStages::empty();
        for (bit, v) in [
            (Self::VERTEX, MTLStages::Vertex),
            (Self::FRAGMENT, MTLStages::Fragment),
            (Self::DISPATCH, MTLStages::Dispatch),
            (Self::BLIT, MTLStages::Blit),
        ] {
            if self.0 & bit.0 != 0 {
                s |= v;
            }
        }
        s
    }

    fn only(self, allowed: Self) -> bool {
        self.0 != 0 && self.0 & !allowed.0 == 0
    }
}

impl BitOr for Stages {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// What a barrier makes visible.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Visibility {
    /// Nothing. The barrier only orders execution, which is enough after a read.
    None,
    /// Writes reach device memory, so later reads in any stage see them.
    Device,
    /// Writes are visible through other virtual addresses of the same memory.
    ResourceAlias,
}

impl Visibility {
    fn to_mtl(self) -> MTL4VisibilityOptions {
        match self {
            Self::None => MTL4VisibilityOptions::None,
            Self::Device => MTL4VisibilityOptions::Device,
            Self::ResourceAlias => MTL4VisibilityOptions::ResourceAlias,
        }
    }
}

/// How a pass or dispatch uses a resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Use {
    /// A compute kernel writes it.
    ComputeWrite,
    /// A compute kernel reads it.
    ComputeRead,
    /// A render pass writes it as a colour or depth attachment.
    AttachmentWrite,
    /// A vertex shader reads it (vertex data, indices, a texture).
    VertexRead,
    /// A fragment shader reads it (a texture, a buffer).
    FragmentRead,
    /// A copy writes it.
    CopyWrite,
    /// A copy reads it.
    CopyRead,
}

impl Use {
    /// The stage in which the use happens.
    pub fn stages(self) -> Stages {
        match self {
            Self::ComputeWrite | Self::ComputeRead => Stages::DISPATCH,
            Self::AttachmentWrite | Self::FragmentRead => Stages::FRAGMENT,
            Self::VertexRead => Stages::VERTEX,
            Self::CopyWrite | Self::CopyRead => Stages::BLIT,
        }
    }

    /// True when the use changes the resource.
    pub fn writes(self) -> bool {
        matches!(
            self,
            Self::ComputeWrite | Self::AttachmentWrite | Self::CopyWrite
        )
    }
}

/// Where a barrier sits relative to the passes it orders.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    /// The commands after the barrier in this pass wait for the `after` stages of earlier passes.
    FromEarlierPasses,
    /// The commands after the barrier in this pass wait for the `after` stages of earlier commands of this pass.
    WithinPass,
    /// Later passes wait for the `after` stages of this pass and the passes before it.
    ToLaterPasses,
}

/// A barrier: the commands in the `before` stages wait until the work in the `after` stages is done.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Dependency {
    /// Which work waits for which.
    pub scope: Scope,
    /// The stages of the work that must finish first.
    pub after: Stages,
    /// The stages of the work that waits.
    pub before: Stages,
    /// What the barrier makes visible.
    pub visibility: Visibility,
}

impl Dependency {
    /// The barrier that a pass starts with when it uses what an earlier pass produced.
    ///
    /// Reads after a write see the write (`Visibility::Device`). A write after a read only needs the ordering.
    pub fn between(producer: Use, consumer: Use) -> Self {
        Self {
            scope: Scope::FromEarlierPasses,
            after: producer.stages(),
            before: consumer.stages(),
            visibility: if producer.writes() {
                Visibility::Device
            } else {
                Visibility::None
            },
        }
    }

    /// The same dependency between two commands of one pass.
    pub fn within_pass(producer: Use, consumer: Use) -> Self {
        Self {
            scope: Scope::WithinPass,
            ..Self::between(producer, consumer)
        }
    }

    pub(crate) fn encode<E: MTL4CommandEncoder + Message>(
        &self,
        enc: &E,
        allowed: Stages,
    ) -> Result<()> {
        let local = match self.scope {
            Scope::FromEarlierPasses => self.before,
            Scope::WithinPass => self.before | self.after,
            Scope::ToLaterPasses => self.after,
        };
        if !local.only(allowed) || self.after.0 == 0 || self.before.0 == 0 {
            return Err(MetalError::Invalid(
                "the barrier names a stage this pass cannot run",
            ));
        }
        let (after, before, vis) = (
            self.after.to_mtl(),
            self.before.to_mtl(),
            self.visibility.to_mtl(),
        );
        match self.scope {
            Scope::FromEarlierPasses => {
                enc.barrierAfterQueueStages_beforeStages_visibilityOptions(after, before, vis)
            }
            Scope::WithinPass => enc
                .barrierAfterEncoderStages_beforeEncoderStages_visibilityOptions(
                    after, before, vis,
                ),
            Scope::ToLaterPasses => {
                enc.barrierAfterStages_beforeQueueStages_visibilityOptions(after, before, vis)
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_after_a_write_flushes_and_a_write_after_a_read_only_orders() {
        let d = Dependency::between(Use::AttachmentWrite, Use::ComputeRead);
        assert_eq!(
            (d.after, d.before, d.visibility),
            (Stages::FRAGMENT, Stages::DISPATCH, Visibility::Device)
        );
        let d = Dependency::between(Use::FragmentRead, Use::AttachmentWrite);
        assert_eq!(d.visibility, Visibility::None);
        assert_eq!(
            Dependency::within_pass(Use::ComputeWrite, Use::CopyRead).scope,
            Scope::WithinPass
        );
    }

    #[test]
    fn stage_sets_are_checked() {
        assert!((Stages::VERTEX | Stages::FRAGMENT).only(Stages::ALL));
        assert!(!Stages::BLIT.only(Stages::VERTEX | Stages::FRAGMENT));
        assert!(!Stages::default().only(Stages::ALL));
    }
}
