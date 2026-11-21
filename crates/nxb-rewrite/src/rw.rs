//! The rewriter's state (the module-level variables of `patch_nso.py`): the image, the layout, the patch areas
//! and the helpers every rule uses.

use nxb_a64::{Insn, Op, Reg, decode, encode};

use crate::error::{RewriteError, fail};

/// Result of a rewrite step.
pub(crate) type R<T> = Result<T, RewriteError>;

/// Key page slots (index into the key page).
pub(crate) const K18: u32 = 0;
pub(crate) const KTLS: u32 = 1;
pub(crate) const KTLR: u32 = 2;
pub(crate) const KGATE: u32 = 3;

/// One gate table row: return point, kind, argument, original address.
pub(crate) struct Gate {
    pub(crate) ret: i64,
    pub(crate) kind: String,
    pub(crate) arg: i64,
    pub(crate) orig: i64,
}

/// The state of one rewrite.
pub(crate) struct Rw {
    pub(crate) name: String,
    pub(crate) img: Vec<u8>,
    pub(crate) text_end: i64,
    pub(crate) far: bool,
    pub(crate) front: i64,
    pub(crate) big_text: bool,
    pub(crate) two: bool,
    pub(crate) split: i64,
    pub(crate) key_page_off: i64,
    pub(crate) image_size: i64,
    pub(crate) patch_off: i64,
    pub(crate) back_off: i64,
    pub(crate) file_image_len: usize,
    pub(crate) ro_off: usize,
    pub(crate) data_off: usize,
    pub(crate) front_area: Vec<u8>,
    pub(crate) back_area: Vec<u8>,
    pub(crate) in_back: bool,
    pub(crate) gates: Vec<Gate>,
    pub(crate) nfix: usize,
    pub(crate) stats: Vec<(String, usize)>,
}

/// Adapts an encoder's reach error.
pub(crate) fn reach<T>(r: Result<T, encode::Reach>) -> R<T> {
    r.or_else(|e| fail(format!("assertion failed: {}", e.0)))
}

impl Rw {
    /// The name of the function starting at `fa` (its symbol), or its offset.
    pub(crate) fn function_name(&self, fa: i64) -> String {
        crate::dynsym::defined(&self.img)
            .into_iter()
            .find(|(_, (v, _))| *v == fa)
            .map_or_else(|| format!("at {fa:#x}"), |(n, _)| n)
    }

    /// Counts a rule's use.
    pub(crate) fn stat(&mut self, k: &str) {
        if let Some(e) = self.stats.iter_mut().find(|e| e.0 == k) {
            e.1 += 1;
        } else {
            self.stats.push((k.to_string(), 1));
        }
    }

    /// Emits what follows into the patch area that reaches the code at offset `a`.
    pub(crate) fn use_area(&mut self, a: i64) {
        self.in_back = self.two && a >= self.split;
    }

    fn area(&self) -> &Vec<u8> {
        if self.in_back {
            &self.back_area
        } else {
            &self.front_area
        }
    }

    /// The offset the next emitted word will have.
    pub(crate) fn here(&self) -> i64 {
        let base = if self.in_back {
            self.back_off
        } else {
            self.patch_off
        };
        base + self.area().len() as i64
    }

    /// Appends an instruction word.
    pub(crate) fn emit(&mut self, w: u32) {
        let a = if self.in_back {
            &mut self.back_area
        } else {
            &mut self.front_area
        };
        a.extend_from_slice(&w.to_le_bytes());
    }

    /// `b` from here to `to`.
    pub(crate) fn emit_b(&mut self, to: i64) -> R<()> {
        let w = reach(encode::b(self.here(), to))?;
        self.emit(w);
        Ok(())
    }

    /// Writes `b to` over the instruction at `at` in the image.
    pub(crate) fn put_branch(&mut self, at: i64, to: i64) -> R<()> {
        let w = reach(encode::b(at, to))?;
        self.put32(at, w);
        Ok(())
    }

    /// The word at image offset `o`.
    pub(crate) fn w32(&self, o: i64) -> u32 {
        let o = o as usize;
        self.img
            .get(o..o + 4)
            .map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }

    /// Writes a word into the image.
    pub(crate) fn put32(&mut self, o: i64, w: u32) {
        let o = o as usize;
        if let Some(s) = self.img.get_mut(o..o + 4) {
            s.copy_from_slice(&w.to_le_bytes());
        }
    }

    /// Emits `adrp reg` to the key page and `ldr reg, [reg, #slot*8]`.
    pub(crate) fn emit_key_addr(&mut self, reg: u32, kind: u32) -> R<()> {
        self.nfix += 1;
        let w = reach(encode::adrp(reg, self.here(), self.key_page_off))?;
        self.emit(w);
        self.emit(encode::ldrx(reg, reg, kind));
        Ok(())
    }

    /// `base += thread slot offset`, via `key_reg`.
    pub(crate) fn emit_slot_addr(&mut self, base: u32, key_reg: u32, kind: u32) -> R<()> {
        self.emit_key_addr(key_reg, kind)?;
        self.emit(encode::addr(base, base, key_reg));
        Ok(())
    }
}

/// `dis`: exactly one instruction decodes.
pub(crate) fn dis(w: u32, addr: i64) -> R<Insn> {
    decode(w, addr as u64)
        .ok_or_else(|| RewriteError::Failed(format!("assertion failed: {w:#x} does not decode")))
}

/// Replaces `x18` / `w18` by `s` in the operands.
fn replace18(o: &Op, s: u8) -> Op {
    let fix = |r: &Reg| match r {
        Reg::X(18) => Reg::X(s),
        Reg::W(18) => Reg::W(s),
        r => *r,
    };
    match o {
        Op::Reg(r, sh) => Op::Reg(fix(r), *sh),
        Op::Mem {
            base,
            index,
            disp,
            wb,
        } => Op::Mem {
            base: fix(base),
            index: index.map(|(r, sh)| (fix(&r), sh)),
            disp: *disp,
            wb: *wb,
        },
        o => o.clone(),
    }
}

/// `x18` / `w18` → `xS` / `wS`: only the four register field positions are changed, verified against the decoding.
pub(crate) fn subst(ins: &Insn, s: u32) -> R<u32> {
    let w = ins.word;
    let want: Vec<Op> = ins.ops.iter().map(|o| replace18(o, s as u8)).collect();
    let mut pos: Vec<u32> = [0u32, 5, 10, 16]
        .into_iter()
        .filter(|p| (w >> p) & 31 == 18)
        .collect();
    if ins.simd {
        // only fields that hold a general register: changing the field changes exactly such operands
        pos.retain(|p| {
            let nw = (w & !(31 << p)) | (s << p);
            decode(nw, ins.addr).is_some_and(|dd| {
                dd.mn == ins.mn
                    && dd.ops.len() == ins.ops.len()
                    && dd.ops != ins.ops
                    && dd
                        .ops
                        .iter()
                        .zip(&ins.ops)
                        .all(|(n, o)| n == o || *n == replace18(o, s as u8))
            })
        });
    }
    for k in (1..=pos.len()).rev() {
        let mut found = None;
        combos(&pos, k, &mut Vec::new(), &mut |c: &[u32]| {
            if found.is_some() {
                return;
            }
            let mut nw = w;
            for p in c {
                nw = (nw & !(31 << p)) | (s << p);
            }
            if let Some(dd) = decode(nw, ins.addr)
                && dd.mn == ins.mn
                && dd.ops == want
            {
                found = Some(nw);
            }
        });
        if let Some(nw) = found {
            return Ok(nw);
        }
    }
    fail(format!(
        "subst fail {:x} {} {}",
        ins.addr,
        ins.mn,
        ins.op_str()
    ))
}

/// `itertools.combinations(items, k)` in its order.
fn combos(items: &[u32], k: usize, cur: &mut Vec<u32>, f: &mut impl FnMut(&[u32])) {
    if cur.len() == k {
        f(cur);
        return;
    }
    let start = cur.len();
    let _ = start;
    fn rec(items: &[u32], from: usize, k: usize, cur: &mut Vec<u32>, f: &mut impl FnMut(&[u32])) {
        if cur.len() == k {
            f(cur);
            return;
        }
        for i in from..items.len() {
            cur.push(items[i]);
            rec(items, i + 1, k, cur, f);
            cur.pop();
        }
    }
    rec(items, 0, k, cur, f);
}

/// The number of a general register as Capstone's `reg_name` spells it (`fp` and `lr` have none).
pub(crate) fn num(r: &Reg) -> Option<u32> {
    match r {
        Reg::X(29) | Reg::X(30) => None,
        Reg::X(n) | Reg::W(n) => Some(u32::from(*n)),
        _ => None,
    }
}

/// The general registers `regs_in` collects: those the text names and those `regs_access` lists.
pub(crate) fn regs_in(ins: &Insn) -> Vec<u32> {
    let mut s: Vec<u32> = ins.text_gprs().into_iter().map(u32::from).collect();
    for r in ins.reads.iter().chain(ins.writes.iter()) {
        s.extend(num(r));
    }
    s.sort_unstable();
    s.dedup();
    s
}

/// The first `n` registers 0..16 that are not in `avoid` and not 18.
pub(crate) fn pick(avoid: &[u32], n: usize) -> R<Vec<u32>> {
    let out: Vec<u32> = (0..16)
        .filter(|r| !avoid.contains(r) && *r != 18)
        .take(n)
        .collect();
    if out.len() != n {
        return fail("assertion failed: pick");
    }
    Ok(out)
}
