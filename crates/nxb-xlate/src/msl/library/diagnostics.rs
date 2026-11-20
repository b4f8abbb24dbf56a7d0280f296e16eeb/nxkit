//! Diagnostic-only shader library entry points.

use super::{Context, Mode, Options, R, Structured, Translated, build_with};

/// Build a diagnostic domain library without enabling it in the production producer.
pub(in crate::msl) fn build_domain(
    s: &Structured,
    ctx: &Context<'_>,
    opt: &Options,
) -> R<Translated> {
    build_with(s, ctx, opt, &[], Mode::Domain)
}

pub(in crate::msl) fn build_indexed_capture(
    s: &Structured,
    ctx: &Context<'_>,
    opt: &Options,
) -> R<Translated> {
    build_with(s, ctx, opt, &[], Mode::IndexedCapture)
}
