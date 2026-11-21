//! Why a draw, copy or dispatch was not encoded, and the two helpers that note a texture or buffer miss. A leaf of the
//! crate: every layer reports a [`Skip`] (W29).

/// Why a draw, copy or dispatch was not encoded. The encoder counts each reason, so a missing feature shows up as a
/// number and not as a silent hole in the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Skip {
    /// The frame has no detail for the command.
    Detail,
    /// The shader library or the reflection is missing or not readable.
    Program,
    /// The reflection has a binding kind that the encoder does not fill.
    Binding,
    /// The primitive type has no Metal equivalent (fan, quads, adjacency, patches).
    Primitive,
    /// The index type has no Metal equivalent (byte indices).
    Index,
    /// A vertex or texture format has no Metal equivalent.
    Format,
    /// The render targets leave a gap between colour slots, or the pass has no target.
    Target,
    /// Front and back faces are both culled.
    Cull,
    /// A texture could not be created or resolved.
    Texture,
    /// The pipeline did not compile.
    Pipeline,
    /// A buffer address is outside guest memory, or the scratch buffer is full.
    Buffer,
    /// A copy between a scaled and an unscaled texture, or between different formats.
    Copy,
    /// A texture upload or copy-back failed.
    Transfer,
    /// Metal refused an encoded command.
    Encode,
    /// The pipeline is still compiling on a background thread. The draw is left out until it is ready.
    Pending,
    /// The draw's program is listed in `NXKIT_SKIP_PROGRAMS` or `NXKIT_SKIP_FILE`.
    Listed,
}

impl Skip {
    /// The number of reasons.
    pub const COUNT: usize = 16;

    /// Every reason, in index order.
    pub const ALL: [Self; Self::COUNT] = [
        Self::Detail,
        Self::Program,
        Self::Binding,
        Self::Primitive,
        Self::Index,
        Self::Format,
        Self::Target,
        Self::Cull,
        Self::Texture,
        Self::Pipeline,
        Self::Buffer,
        Self::Copy,
        Self::Transfer,
        Self::Encode,
        Self::Pending,
        Self::Listed,
    ];

    /// The reason as a short name for logs.
    pub fn name(self) -> &'static str {
        match self {
            Self::Detail => "detail",
            Self::Program => "program",
            Self::Binding => "binding",
            Self::Primitive => "primitive",
            Self::Index => "index",
            Self::Format => "format",
            Self::Target => "target",
            Self::Cull => "cull",
            Self::Texture => "texture",
            Self::Pipeline => "pipeline",
            Self::Buffer => "buffer",
            Self::Copy => "copy",
            Self::Transfer => "transfer",
            Self::Encode => "encode",
            Self::Pending => "pending",
            Self::Listed => "listed",
        }
    }
}

/// Prints a line of category `tex` (at most 300) and gives back `Skip::Texture`. The health report keeps it as an
/// example of the skip.
pub(crate) fn tex_skip(what: std::fmt::Arguments<'_>) -> Skip {
    static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    nxkit_nvn::diag::example(nxkit_nvn::diag::Reason::SkipTexture, what);
    if crate::diag("tex") && crate::diag::first(&SHOWN, 300) {
        eprintln!("DIAG tex {what}");
    }
    Skip::Texture
}

/// Prints a line of category `buf` (at most 300) and gives back `Skip::Buffer`. The health report keeps it as an
/// example of the skip.
pub(crate) fn buf_skip(what: std::fmt::Arguments<'_>) -> Skip {
    static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    nxkit_nvn::diag::example(nxkit_nvn::diag::Reason::SkipBuffer, what);
    if crate::diag("buf") && crate::diag::first(&SHOWN, 300) {
        eprintln!("DIAG buf {what}");
    }
    Skip::Buffer
}
