//! The Ampere (sm_86) emitters: `InstEmitAmpere*.cs`. `dispatch` maps the table's `AEmit` to the emitter.

mod bits;
mod float;
mod flow;
mod half;
mod hmma;
mod int;
mod mem;
mod mov;
pub mod operand;
pub mod ops;
mod surface;
pub(crate) mod tensor;
mod tex;

use crate::emit::emitter::EmitterContext;
use crate::maxwell::ampere_table::AEmit;

/// Run the emitter of the current op.
pub fn dispatch(ctx: &mut EmitterContext<'_, '_>, e: AEmit) -> Result<(), String> {
    use AEmit::*;
    match e {
        ABra => flow::bra(ctx),
        ABrx => flow::brx(ctx),
        AExit => flow::exit(ctx),
        AKill => flow::kill(ctx),
        ACall => flow::call(ctx),
        ARet => flow::ret(ctx),
        ANop => flow::nop(ctx),
        AFadd => float::fadd(ctx),
        AFmul => float::fmul(ctx),
        AFfma => float::ffma(ctx),
        AFmnmx => float::fmnmx(ctx),
        AFset => float::fset(ctx),
        AFsetp => float::fsetp(ctx),
        AFsel => float::fsel(ctx),
        AMufu => float::mufu(ctx),
        AF2i => float::f2i(ctx),
        AI2f => float::i2f(ctx),
        AF2i64 => float::f2i64(ctx),
        AI2f64 => float::i2f64(ctx),
        AI2fp => float::i2fp(ctx),
        AFrnd => float::frnd(ctx),
        AF2f => float::f2f(ctx),
        AF2fp => float::f2fp(ctx),
        AF2ip => float::f2ip(ctx),
        AFswzadd => float::fswzadd(ctx),
        AHadd2 => half::hadd2(ctx),
        AHmul2 => half::hmul2(ctx),
        AHfma2 => half::hfma2(ctx),
        AMov => mov::mov(ctx),
        AUMov => mov::umov(ctx),
        AR2ur => mov::r2ur(ctx),
        ASel => mov::sel(ctx),
        ACs2r => mov::cs2r(ctx),
        AS2r => mov::s2r(ctx),
        AUldc => mov::uldc(ctx),
        ALdc => mov::ldc(ctx),
        AR2p => mov::r2p(ctx),
        AP2r => mov::p2r(ctx),
        AUp2ur => mov::up2ur(ctx),
        APlop3 => mov::plop3(ctx),
        AVote => mov::vote(ctx),
        AShfl => mov::shfl(ctx),
        AIsetp => int::isetp(ctx),
        AIadd3 => int::iadd3(ctx),
        AImad => int::imad(ctx),
        AImadWide => int::imad_wide(ctx),
        AImadHi => int::imad_hi(ctx),
        ALop3 => int::lop3(ctx),
        AShf => int::shf(ctx),
        ALea => int::lea(ctx),
        AImnmx => int::imnmx(ctx),
        AIabs => int::iabs(ctx),
        APrmt => bits::prmt(ctx),
        APopc => bits::popc(ctx),
        AFlo => bits::flo(ctx),
        ABrev => bits::brev(ctx),
        ASgxt => bits::sgxt(ctx),
        ABmsk => bits::bmsk(ctx),
        AGuardCull => mov::guard_cull(ctx),
        APixelCovered => mov::pixel_covered(ctx),
        AIpa => mem::ipa(ctx),
        AAld => mem::ald(ctx),
        AAst => mem::ast(ctx),
        AIsberd => mem::isberd(ctx),
        AStageOut => mem::stage_out(ctx),
        ALepc => mem::lepc(ctx),
        ALdg => mem::ldg(ctx),
        AStg => mem::stg(ctx),
        ARed => mem::red(ctx),
        AAtomg => mem::atomg(ctx),
        AAtomgCas => mem::atomg_cas(ctx),
        AMembar => mem::membar(ctx),
        AErrbar => Ok(()),
        ABar => mem::bar(ctx),
        ALdLocalShared => mem::ld_local_shared(ctx),
        AStLocalShared => mem::st_local_shared(ctx),
        AAtoms => mem::atoms(ctx),
        AAtomsCas => mem::atoms_cas(ctx),
        ATex => tex::tex(ctx),
        ATxq => tex::txq(ctx),
        ASurface => surface::surface(ctx),
        ASuatom => surface::suatom(ctx),
        ASured => surface::sured(ctx),
        ARedux => tensor::redux(ctx),
        AMatch => tensor::match_(ctx),
        AHsetp2 => tensor::hsetp2(ctx),
        AHset2 => tensor::hset2(ctx),
        AHmnmx2 => tensor::hmnmx2(ctx),
        AMovm => tensor::movm(ctx),
        ALdsm => tensor::ldsm(ctx),
        AHmma => hmma::hmma(ctx),
        other => Err(format!("unported: Ampere emitter {other:?}")),
    }
}
