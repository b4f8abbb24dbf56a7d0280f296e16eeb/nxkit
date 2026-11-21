//! The primitives that every reader of the split shares: the magics, the stage names, a found shader ([`Found`]), the
//! checks of a control section and code area, and the byte search. The readers of the container formats and the scan
//! of `crate::shaders` build on them, so they sit below both.

use crate::SplitError;

/// Control section magic.
pub const CTL_MAGIC: [u8; 4] = [0x34, 0x12, 0x76, 0x98];

pub(crate) const GFX_MAGIC: [u8; 4] = [0x78, 0x56, 0x34, 0x12];

pub(crate) const CS_MAGIC: [u8; 4] = [0x79, 0x56, 0x34, 0x12];

/// BNSH stage order.
pub const STAGES: [&str; 6] = ["vs", "hs", "ds", "gs", "ps", "cs"];

pub(crate) fn u32le(d: &[u8], o: usize) -> Option<u32> {
    d.get(o..o.checked_add(4)?)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// One shader found: `(where, stage, control, code)` of the Python.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// Where (label and position).
    pub place: String,
    /// Stage (`vs`, `ps`, `cs`, ...).
    pub stage: &'static str,
    /// Control section.
    pub ctl: Vec<u8>,
    /// Code area.
    pub code: Vec<u8>,
}

pub(crate) fn nvn2_ctl(ctl: &[u8]) -> bool {
    ctl.len() >= 0x48 && ctl[..4] == CTL_MAGIC && ctl[4..8] == [0xc9, 0, 0, 0]
}

pub(crate) fn ctl_len(ctl: &[u8]) -> usize {
    if nvn2_ctl(ctl) {
        return 0x48;
    }
    let n = u32le(ctl, 0x20).unwrap_or(0) as usize;
    if (0x760..0x880).contains(&n) {
        n
    } else {
        0x880
    }
}

/// `check`: the reason a control section and code area do not fit, or `None`.
pub fn check(st: &str, ctl: &[u8], code: &[u8]) -> Option<&'static str> {
    if ctl.len() < 0x24 || ctl[..4] != CTL_MAGIC || ctl.len() < ctl_len(ctl) {
        return Some("bad control section");
    }
    if nvn2_ctl(ctl) {
        return if u32le(ctl, 0x44) == Some(code.len() as u32) {
            None
        } else {
            Some("control +0x44 does not match the code area length")
        };
    }
    let head = code.get(..4);
    let bad_cs = st == "cs"
        && (head == Some(&CS_MAGIC[..]) || head == Some(&GFX_MAGIC[..]))
        && head != Some(&CS_MAGIC[..]);
    if bad_cs || (st != "cs" && head != Some(&GFX_MAGIC[..])) {
        return Some("code area header magic does not match the stage");
    }
    if u32le(ctl, 0x704) != Some(code.len() as u32) {
        return Some("control +0x704 does not match the code area length");
    }
    None
}

pub(crate) fn sph_stage(code: &[u8]) -> Option<&'static str> {
    let w = u32le(code, 0x30)?;
    if (w >> 10) & 0xf == 0 && w & 0x1f == 1 {
        return Some("vs");
    }
    match (w >> 10) & 0xf {
        1 => Some("vs"),
        2 => Some("hs"),
        3 => Some("ds"),
        4 => Some("gs"),
        5 => Some("ps"),
        _ => None,
    }
}

pub(crate) fn span<'a>(
    d: &'a [u8],
    at: usize,
    n: usize,
    place: &str,
) -> Result<&'a [u8], SplitError> {
    d.get(at..at.saturating_add(n)).ok_or_else(|| {
        SplitError::Red(format!(
            "{place}: {n:#x} bytes at {at:#x} lie outside the {:#x} bytes there are",
            d.len()
        ))
    })
}

pub(crate) fn find_all<'a>(d: &'a [u8], pat: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
    // non-overlapping left-to-right matches, as re.finditer
    let mut i = 0usize;
    std::iter::from_fn(move || {
        if i + pat.len() > d.len() {
            return None;
        }
        let p = memfind(&d[i..], pat)? + i;
        i = p + pat.len();
        Some(p)
    })
}

/// Position of `pat` in `d`.
pub fn memfind(d: &[u8], pat: &[u8]) -> Option<usize> {
    let first = *pat.first()?;
    let mut i = 0;
    while i + pat.len() <= d.len() {
        let p = d[i..=d.len() - pat.len()]
            .iter()
            .position(|&b| b == first)?
            + i;
        if &d[p..p + pat.len()] == pat {
            return Some(p);
        }
        i = p + 1;
    }
    None
}
