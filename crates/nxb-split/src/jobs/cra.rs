//! Camelot's CRA archive (`cra_jobs` and `cra_shaders` of the reference): every file in ranges of about equal
//! length, each unscrambled, unpacked and scanned like a loose file. The index comes from an NRO of the game.

use std::sync::Arc;

use super::{Ctx, File, Job, JobOut, NroSet, Plan, R, ranges};
use crate::SplitError;
use crate::bahs::BahsVar;
use crate::cra::{self, Entry, Index};
use crate::narc;
use crate::shaders::{Found, scan_blob};
use crate::src::Src;

/// The first four bytes of a CRA archive.
pub const MAGIC: &[u8; 4] = b"CRA.";

/// A sound (FSTM) or a movie (EBML): left out by its first block, as unscrambling is the slow part.
fn skipped(head: &[u8]) -> bool {
    head.get(..4) == Some(b"FSTM") || head.get(..4) == Some(&[0x1a, 0x45, 0xdf, 0xa3])
}

fn shaders(s: &dyn Src, rel: &str, ents: &[Entry]) -> R<JobOut> {
    let (mut out, mut var) = (Vec::<Found>::new(), BahsVar::new());
    for e in ents {
        if skipped(&cra::head(&s.read_up(e.off, 16)?, e)) {
            continue;
        }
        let plain = cra::read(&s.read_up(e.off, cra::span(e))?, e)?;
        let d = narc::undo(&plain)?;
        scan_blob(&d, &format!("/{rel}/{}", e.label()), &mut out, &mut var, 0)?;
    }
    Ok(JobOut { found: out, var })
}

/// The files of the archive, by the first NRO that holds the file table.
fn entries(rel: &str, nros: &NroSet) -> R<Vec<Entry>> {
    for n in &nros.files {
        if let Some(ix) = Index::new(n)? {
            return ix.files();
        }
    }
    Err(SplitError::Red(format!(
        "/{rel}: a CRA archive, and no NRO holds its file table"
    )))
}

/// `cra_jobs`.
pub fn plan(fs_: &[File], nros: &NroSet, p: &mut Plan) -> R<()> {
    for (i, f) in fs_.iter().enumerate() {
        if !f.live || f.head(4)? != MAGIC {
            continue;
        }
        let es = entries(&f.rel, nros)?;
        let step = es.iter().map(|e| u64::from(e.size)).sum::<u64>() as f64 / 64.0;
        for chunk in ranges(&es, |e| u64::from(e.size), step) {
            let (src, rel): (Arc<dyn Src>, String) = (f.src.clone(), f.rel.clone());
            let r2 = rel.clone();
            p.jobs.push(Job {
                rel,
                run: Box::new(move |_: &Ctx<'_>, _| shaders(src.as_ref(), &r2, &chunk)),
            });
        }
        p.taken.insert(i);
    }
    Ok(())
}
