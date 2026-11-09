//! The levels and slices of a texture that a frame wrote.

/// The levels and slices of a texture that a frame wrote, for its copy-back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Dirty {
    /// Every level and slice (a dispatch, a copy, a 3D attachment).
    All,
    /// These (level, slice) pairs: the attachment views of the render passes.
    Parts(Vec<(u32, u32)>),
}

impl Default for Dirty {
    fn default() -> Self {
        Self::Parts(Vec::new())
    }
}

impl Dirty {
    /// Adds one level and slice.
    pub(crate) fn add(&mut self, level: u32, slice: u32) {
        if let Self::Parts(v) = self
            && !v.contains(&(level, slice))
        {
            v.push((level, slice));
        }
    }

    /// True when nothing was written.
    pub(crate) fn is_empty(&self) -> bool {
        matches!(self, Self::Parts(v) if v.is_empty())
    }

    /// The (level, slice) pairs inside a texture of `levels` levels and `slices` slices, in order.
    pub(crate) fn parts(&self, levels: u32, slices: u32) -> Vec<(u32, u32)> {
        match self {
            Self::All => (0..slices)
                .flat_map(|s| (0..levels).map(move |l| (l, s)))
                .collect(),
            Self::Parts(v) => {
                let mut v: Vec<_> = v
                    .iter()
                    .copied()
                    .filter(|&(l, s)| l < levels && s < slices)
                    .collect();
                v.sort_unstable_by_key(|&(l, s)| (s, l));
                v
            }
        }
    }
}
