//! Specialized shader library entry points.

use super::{Context, Mode, Options, R, Structured, Translated, build_with};

/// Build domain entries; only the diagnostic path advertises its explicitly linked ABI.
pub(in crate::msl) fn build_domain(
    s: &Structured,
    ctx: &Context<'_>,
    opt: &Options,
    diagnostic: bool,
) -> R<Translated> {
    build_with(s, ctx, opt, &[], Mode::Domain { diagnostic })
}

pub(in crate::msl) fn build_indexed_capture(
    s: &Structured,
    ctx: &Context<'_>,
    opt: &Options,
) -> R<Translated> {
    build_with(s, ctx, opt, &[], Mode::IndexedCapture)
}
