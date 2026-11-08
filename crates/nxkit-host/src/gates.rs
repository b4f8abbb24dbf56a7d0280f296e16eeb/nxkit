//! The gate table: what each gate block of the rewritten code means, found by its return address.
//!
//! `nxb-rewrite` writes `<name>.gates.tsv` with one row per gate block: `ret`, `kind`, `arg`, `orig`. `ret` is the
//! address after the `blr` of the block, relative to the module base, in hex with an optional `-`. The gate gets this
//! address in `x30`.

use crate::error::{HostError, Result};

/// What a gate block replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `svc #n`.
    Svc(u32),
    /// `mrs xN, cntpct_el0` or `cntvct_el0`. The system tick goes into register `N`.
    Counter(u8),
    /// An observation gate. Nothing observes yet, so the pass does nothing.
    Obs,
    /// A pass of a spin wait. The thread yields.
    Spin,
}

/// One gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gate {
    /// The absolute return address.
    pub ret: u64,
    /// The meaning.
    pub kind: Kind,
}

fn bad(line: usize, m: &str) -> HostError {
    HostError::Manifest(format!("gates file, line {line}: {m}"))
}

fn parse_signed_hex(s: &str) -> Option<i64> {
    let (neg, rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let digits = rest.strip_prefix("0x").unwrap_or(rest);
    let v = i64::from_str_radix(digits, 16).ok()?;
    Some(if neg { -v } else { v })
}

/// Reads the rows of a gates file for the module that sits at `base`.
pub fn parse(src: &str, base: u64) -> Result<Vec<Gate>> {
    let mut out = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let n = i + 1;
        if line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 3 {
            return Err(bad(n, "fewer than three columns"));
        }
        let rel = parse_signed_hex(cols[0]).ok_or_else(|| bad(n, "ret is not a hex number"))?;
        let arg: i64 = cols[2].parse().map_err(|_| bad(n, "arg is not a number"))?;
        let kind = match cols[1] {
            "svc" => Kind::Svc(u32::try_from(arg).map_err(|_| bad(n, "svc number out of range"))?),
            "cntpct" | "cntvct" => {
                let r = u8::try_from(arg).map_err(|_| bad(n, "register out of range"))?;
                if r > 30 {
                    return Err(bad(n, "register out of range"));
                }
                Kind::Counter(r)
            }
            "obs" => Kind::Obs,
            "spin" => Kind::Spin,
            other => return Err(bad(n, &format!("unknown gate kind {other}"))),
        };
        let ret = base
            .checked_add_signed(rel)
            .ok_or_else(|| bad(n, "ret is outside the address space"))?;
        out.push(Gate { ret, kind });
    }
    Ok(out)
}

/// An open addressing hash table over the return addresses. It is built once and read by every guest thread.
#[derive(Debug, Default)]
pub struct GateTable {
    slots: Vec<Option<Gate>>,
}

fn hash(ret: u64, mask: usize) -> usize {
    ((ret >> 2).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) as usize & mask
}

impl GateTable {
    /// Builds the table. Two gates with one return address are an error.
    pub fn build(gates: &[Gate]) -> Result<GateTable> {
        let size = (gates.len() * 2).max(16).next_power_of_two();
        let mask = size - 1;
        let mut slots: Vec<Option<Gate>> = vec![None; size];
        for g in gates {
            let mut i = hash(g.ret, mask);
            loop {
                match slots[i] {
                    None => {
                        slots[i] = Some(*g);
                        break;
                    }
                    Some(e) if e.ret == g.ret => {
                        return Err(HostError::Manifest(format!(
                            "two gates return to {:#x}",
                            g.ret
                        )));
                    }
                    Some(_) => i = (i + 1) & mask,
                }
            }
        }
        Ok(GateTable { slots })
    }

    /// The gate that returns to `ret`.
    pub fn find(&self, ret: u64) -> Option<Kind> {
        if self.slots.is_empty() {
            return None;
        }
        let mask = self.slots.len() - 1;
        let mut i = hash(ret, mask);
        loop {
            match self.slots[i] {
                None => return None,
                Some(g) if g.ret == ret => return Some(g.kind),
                Some(_) => i = (i + 1) & mask,
            }
        }
    }

    /// The number of gates.
    pub fn len(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    /// True when there is no gate.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rows_with_negative_offsets() {
        let src = "0x4010\tsvc\t11\t0x4000\n-0x20\tcntpct\t19\t0x10\n0x5000\tobs\t3\t0x20\n0x5010\tspin\t0\t0x30\n";
        let g = parse(src, 0x1_0000_0000);
        assert!(g.is_ok());
        let Ok(g) = g else { return };
        assert_eq!(g.len(), 4);
        assert_eq!(g[0].ret, 0x1_0000_4010);
        assert_eq!(g[0].kind, Kind::Svc(11));
        assert_eq!(g[1].ret, 0x1_0000_0000 - 0x20);
        assert_eq!(g[1].kind, Kind::Counter(19));
    }

    #[test]
    fn finds_every_gate_and_nothing_else() {
        let rows: Vec<Gate> = (0..500u64)
            .map(|i| Gate {
                ret: 0x1_0000_0000 + i * 24,
                kind: Kind::Svc(i as u32),
            })
            .collect();
        let t = GateTable::build(&rows);
        assert!(t.is_ok());
        let Ok(t) = t else { return };
        assert_eq!(t.len(), 500);
        for g in &rows {
            assert_eq!(t.find(g.ret), Some(g.kind));
        }
        assert_eq!(t.find(0x1_0000_0004), None);
    }

    #[test]
    fn refuses_an_unknown_kind_and_a_duplicate() {
        assert!(parse("0x10\tfoo\t1\t0x0\n", 0).is_err());
        let g = Gate {
            ret: 8,
            kind: Kind::Obs,
        };
        assert!(GateTable::build(&[g, g]).is_err());
    }
}
