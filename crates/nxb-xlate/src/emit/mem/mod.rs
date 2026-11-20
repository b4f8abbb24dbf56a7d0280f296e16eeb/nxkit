//! Emitters of this family: attributes, memory, textures and surfaces (see `emit/mod.rs`).

pub(crate) mod attr;
#[cfg(test)]
mod diff_tests;
mod hlp;
pub(crate) mod memory;
mod memory_bits;
pub(crate) mod surface;
pub(crate) mod surface_ldst;
pub(crate) mod tex;
mod tex_entry;
mod tex_misc;
mod texs;

use crate::emit::emitter::EmitterContext;
use crate::maxwell::names::Emit;

/// `None` when this family does not handle `emit`.
#[allow(clippy::match_single_binding)]
pub fn try_emit(ctx: &mut EmitterContext<'_, '_>, emit: Emit) -> Option<Result<(), String>> {
    Some(match emit {
        Emit::Al2p => attr::al2p(ctx),
        Emit::Ald => attr::ald(ctx),
        Emit::Ast => attr::ast(ctx),
        Emit::Ipa => attr::ipa(ctx),
        Emit::Isberd => attr::isberd(ctx),
        Emit::OutR | Emit::OutI | Emit::OutC => attr::out(ctx),
        Emit::Ldc => memory::ldc(ctx),
        Emit::Ldg => memory::ldg(ctx),
        Emit::Ldl => memory::ldl(ctx),
        Emit::Lds => memory::lds(ctx),
        Emit::Stg => memory::stg(ctx),
        Emit::Stl => memory::stl(ctx),
        Emit::Sts => memory::sts(ctx),
        Emit::Atom => memory::atom(ctx),
        Emit::Atoms => memory::atoms(ctx),
        Emit::Red => memory::red(ctx),
        Emit::Tex => tex_entry::tex(ctx),
        Emit::TexB => tex_entry::tex_b(ctx),
        Emit::Texs => tex_entry::texs(ctx, false),
        Emit::TexsF16 => tex_entry::texs(ctx, true),
        Emit::Tld => tex_entry::tld(ctx),
        Emit::TldB => tex_entry::tld_b(ctx),
        Emit::Tlds => tex_entry::tlds(ctx, false),
        Emit::TldsF16 => tex_entry::tlds(ctx, true),
        Emit::Tld4 => tex_entry::tld4(ctx),
        Emit::Tld4B => tex_entry::tld4_b(ctx),
        Emit::Tld4s => tex_entry::tld4s(ctx, false),
        Emit::Tld4sF16 => tex_entry::tld4s(ctx, true),
        Emit::Tmml => tex_entry::tmml(ctx),
        Emit::TmmlB => tex_entry::tmml_b(ctx),
        Emit::Txd => tex_entry::txd(ctx),
        Emit::TxdB => tex_entry::txd_b(ctx),
        Emit::Txq => tex_entry::txq(ctx),
        Emit::TxqB => tex_entry::txq_b(ctx),
        Emit::SuatomB => surface::suatom_b(ctx),
        Emit::Suatom => surface::suatom(ctx),
        Emit::SuatomB2 => surface::suatom_b2(ctx),
        Emit::SuatomCasB => surface::suatom_cas_b(ctx),
        Emit::SuatomCas => surface::suatom_cas(ctx),
        Emit::SuldDB => surface_ldst::suld_d_b(ctx),
        Emit::SuldD => surface_ldst::suld_d(ctx),
        Emit::SuldB => surface_ldst::suld_b(ctx),
        Emit::Suld => surface_ldst::suld(ctx),
        Emit::SuredB => surface::sured_b(ctx),
        Emit::Sured => surface::sured(ctx),
        Emit::SustDB => surface_ldst::sust_d_b(ctx),
        Emit::SustD => surface_ldst::sust_d(ctx),
        Emit::SustB => surface_ldst::sust_b(ctx),
        Emit::Sust => surface_ldst::sust(ctx),
        Emit::AtomCas => memory::not_implemented(ctx, "AtomCas"),
        Emit::AtomsCas => memory::not_implemented(ctx, "AtomsCas"),
        Emit::Cctl => memory::not_implemented(ctx, "Cctl"),
        Emit::Cctll => memory::not_implemented(ctx, "Cctll"),
        Emit::Cctlt => memory::not_implemented(ctx, "Cctlt"),
        Emit::Ld => memory::not_implemented(ctx, "Ld"),
        Emit::St => memory::not_implemented(ctx, "St"),
        Emit::Stp => memory::not_implemented(ctx, "Stp"),
        Emit::Txa => memory::not_implemented(ctx, "Txa"),
        _ => return None,
    })
}
