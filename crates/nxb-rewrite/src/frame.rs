//! The frame step stubs of a main build (`FRAME_STEP` of a game's `patches.py`), replayed from a recorded trace.

use nxb_a64::{Insn, encode};

use crate::branch::unhex;
use crate::error::fail;
use crate::patches::{Cut, FsItem, FsOp, Sym};
use crate::rw::{R, Rw, reach};
use crate::scan::Kind;
use std::collections::BTreeMap;

impl Rw {
    /// Replays the stub of a build, returns whether it has a render-scale stub. The stubs go into the front area,
    /// the state page follows the key page (`state`).
    pub(crate) fn replay_frame_step(
        &mut self,
        ops: &[FsOp],
        state: i64,
        kinds: &BTreeMap<i64, (Kind, i64)>,
        windows: &[Vec<Insn>],
        cuts: &mut Vec<Cut>,
    ) -> R<bool> {
        self.use_area(0);
        let mut labels: Vec<i64> = Vec::new();
        let mut scale = false;
        for op in ops {
            match op {
                FsOp::Site(off, orig) => {
                    let ob = unhex(orig);
                    let at = *off;
                    if self.img.get(at..at + ob.len()) != Some(&ob[..]) {
                        return fail(format!(
                            "assertion failed: frame step original bytes mismatch {at:#x}"
                        ));
                    }
                    let mut o = at as i64;
                    while o < (at + ob.len()) as i64 {
                        let in_win = windows
                            .iter()
                            .any(|w| (w[0].addr as i64) <= o && o < w[w.len() - 1].addr as i64 + 4);
                        if kinds.contains_key(&o) || in_win {
                            return fail(format!(
                                "assertion failed: frame step site is a rewrite site {o:#x}"
                            ));
                        }
                        o += 4;
                    }
                }
                FsOp::Word(w) => self.emit(*w),
                FsOp::Adrp(r, pc, t) => {
                    let w = reach(encode::adrp(
                        *r,
                        ev(pc, &labels, state)?,
                        ev(t, &labels, state)?,
                    ))?;
                    self.emit(w);
                }
                FsOp::B(frm, to, orr) => {
                    let w = reach(encode::b(ev(frm, &labels, state)?, ev(to, &labels, state)?))?;
                    self.emit(w | orr);
                }
                FsOp::Label(n) => {
                    if labels.len() <= *n {
                        labels.resize(*n + 1, i64::MIN);
                    }
                    labels[*n] = self.here();
                }
                FsOp::Put(off, why, items) => {
                    let off = *off;
                    let old: Vec<u8> = self.img[off..off + 4 * items.len()].to_vec();
                    for (i, it) in items.iter().enumerate() {
                        let w = match it {
                            FsItem::Word(w) => *w,
                            FsItem::B(frm, to, orr) => {
                                reach(encode::b(ev(frm, &labels, state)?, ev(to, &labels, state)?))?
                                    | orr
                            }
                        };
                        self.put32((off + 4 * i) as i64, w);
                    }
                    let new = self.img[off..off + 4 * items.len()].to_vec();
                    cuts.push(Cut {
                        off,
                        old: hex(&old),
                        new: hex(&new),
                        why: why.clone(),
                    });
                    self.stat("frame_step");
                }
                FsOp::Scale => scale = true,
            }
        }
        Ok(scale)
    }
}

/// The value of a recorded address once its labels are known.
fn ev(s: &Sym, labels: &[i64], state: i64) -> R<i64> {
    match *s {
        Sym::Abs(a) => Ok(a),
        Sym::State(d) => Ok(state + d),
        Sym::Label(n, d) => match labels.get(n) {
            Some(&p) if p != i64::MIN => Ok(p + d),
            _ => fail("assertion failed: frame step label used before it is set"),
        },
    }
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}
