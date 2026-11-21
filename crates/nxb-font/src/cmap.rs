//! `cmap`: the reading of the best Unicode subtable (format 4 or 12) and the writing of format 4.

use crate::sfnt::{Bytes, Put};
use crate::{Error, Result};
use std::collections::BTreeMap;

/// The Unicode map of a font: the subtable fontTools' `getBestCmap` takes, by platform and encoding.
pub(crate) fn read(cmap: Bytes<'_>) -> Result<BTreeMap<u32, u16>> {
    const BEST: [(u16, u16); 8] = [
        (3, 10),
        (0, 6),
        (0, 4),
        (3, 1),
        (0, 3),
        (0, 2),
        (0, 1),
        (0, 0),
    ];
    let n = cmap.u16(2)? as usize;
    let mut subtables = Vec::with_capacity(n);
    for i in 0..n {
        let rec = 4 + 8 * i;
        subtables.push((
            cmap.u16(rec)?,
            cmap.u16(rec + 2)?,
            cmap.u32(rec + 4)? as usize,
        ));
    }
    for want in BEST {
        for &(platform, encoding, off) in &subtables {
            if (platform, encoding) != want {
                continue;
            }
            match cmap.u16(off)? {
                4 => return format4(cmap, off),
                12 => return format12(cmap, off),
                _ => {}
            }
        }
    }
    Err(Error::new("the font has no Unicode cmap of format 4 or 12"))
}

fn format4(b: Bytes<'_>, at: usize) -> Result<BTreeMap<u32, u16>> {
    let segs = b.u16(at + 6)? as usize / 2;
    let ends = at + 14;
    let starts = ends + 2 * segs + 2;
    let deltas = starts + 2 * segs;
    let ranges = deltas + 2 * segs;
    let mut map = BTreeMap::new();
    for i in 0..segs {
        let end = b.u16(ends + 2 * i)?;
        let start = b.u16(starts + 2 * i)?;
        let delta = b.u16(deltas + 2 * i)?;
        let range = b.u16(ranges + 2 * i)? as usize;
        for c in start..=end {
            if c == 0xffff {
                break;
            }
            let g = if range == 0 {
                c.wrapping_add(delta)
            } else {
                let g = b.u16(ranges + 2 * i + range + 2 * usize::from(c - start))?;
                if g == 0 { 0 } else { g.wrapping_add(delta) }
            };
            if g != 0 {
                map.insert(u32::from(c), g);
            }
        }
    }
    Ok(map)
}

fn format12(b: Bytes<'_>, at: usize) -> Result<BTreeMap<u32, u16>> {
    let groups = b.u32(at + 12)? as usize;
    let mut map = BTreeMap::new();
    for i in 0..groups {
        let g = at + 16 + 12 * i;
        let (start, end, first) = (b.u32(g)?, b.u32(g + 4)?, b.u32(g + 8)?);
        if end < start || end > 0x10ffff {
            return Err(Error::new("bad cmap format 12 group"));
        }
        for c in start..=end {
            let gid = first.wrapping_add(c - start);
            if gid != 0 && gid <= 0xffff {
                map.insert(c, gid as u16);
            }
        }
    }
    Ok(map)
}

/// One segment of format 4: the code points `start..=end` and either a delta or glyphs from the array.
struct Segment {
    start: u16,
    end: u16,
    delta: Option<u16>,
}

/// Splits the map into runs of consecutive code points with one delta, then joins runs into array segments where
/// that is smaller: a segment costs 8 bytes, a code point in the glyph array 2. An array segment over runs `i..j` costs
/// `8 + 2 * (end(j - 1) + 1 - start(i))`, so the cheapest split is one pass with the best `cost(i) - 2 * start(i)`.
fn segments(map: &[(u16, u16)]) -> Vec<Segment> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (i, &(c, g)) in map.iter().enumerate() {
        match runs.last_mut() {
            Some((_, end))
                if {
                    let (pc, pg) = map[*end];
                    pc + 1 == c && g.wrapping_sub(c) == pg.wrapping_sub(pc)
                } =>
            {
                *end = i;
            }
            _ => runs.push((i, i)),
        }
    }
    let n = runs.len();
    let start = |r: usize| i64::from(map[runs[r].0].0);
    let end = |r: usize| i64::from(map[runs[r].1].0);
    // best[j]: the cost of runs 0..j and the first run of its last segment. A segment of one run is a delta segment:
    // as an array it costs more than 8 bytes.
    let mut best = vec![(0i64, 0usize); n + 1];
    let mut low = (i64::MAX, 0usize);
    for j in 1..=n {
        let i = j - 1;
        if best[i].0 - 2 * start(i) < low.0 {
            low = (best[i].0 - 2 * start(i), i);
        }
        let array = 8 + 2 * (end(i) + 1) + low.0;
        best[j] = if array < best[i].0 + 8 {
            (array, low.1)
        } else {
            (best[i].0 + 8, i)
        };
    }
    let mut cuts = Vec::new();
    let mut j = n;
    while j > 0 {
        cuts.push((best[j].1, j));
        j = best[j].1;
    }
    cuts.reverse();
    cuts.into_iter()
        .map(|(i, j)| {
            let (first, last) = (runs[i].0, runs[j - 1].1);
            let (start, g) = map[first];
            Segment {
                start,
                end: map[last].0,
                delta: (i + 1 == j).then(|| g.wrapping_sub(start)),
            }
        })
        .collect()
}

/// A format 4 subtable for the code points of the basic plane (language 0).
pub(crate) fn format4_table(map: &BTreeMap<u32, u16>) -> Result<Vec<u8>> {
    let pairs: Vec<(u16, u16)> = map
        .iter()
        .filter(|&(&c, _)| c < 0xffff)
        .map(|(&c, &g)| (c as u16, g))
        .collect();
    let mut segs = segments(&pairs);
    segs.push(Segment {
        start: 0xffff,
        end: 0xffff,
        delta: Some(1),
    });
    let n = segs.len();
    let mut power = 1usize;
    let mut selector = 0u16;
    while power * 2 <= n {
        power *= 2;
        selector += 1;
    }
    let mut glyphs: Vec<u16> = Vec::new();
    let mut offsets = Vec::with_capacity(n);
    let lookup: BTreeMap<u16, u16> = pairs.iter().copied().collect();
    for (i, s) in segs.iter().enumerate() {
        if s.delta.is_some() {
            offsets.push(0);
            continue;
        }
        offsets.push((2 * (n - i) + 2 * glyphs.len()) as u16);
        for c in s.start..=s.end {
            glyphs.push(lookup.get(&c).copied().unwrap_or(0));
        }
    }
    let len = 16 + 8 * n + 2 * glyphs.len();
    if len > 0xffff {
        return Err(Error::new(format!(
            "cmap format 4 is {len} bytes, more than 65535"
        )));
    }
    let mut out = Vec::with_capacity(len);
    out.u16(4);
    out.u16(len as u16);
    out.u16(0);
    out.u16((2 * n) as u16);
    out.u16((2 * power) as u16);
    out.u16(selector);
    out.u16((2 * n - 2 * power) as u16);
    for s in &segs {
        out.u16(s.end);
    }
    out.u16(0);
    for s in &segs {
        out.u16(s.start);
    }
    for s in &segs {
        out.u16(s.delta.unwrap_or(0));
    }
    for o in offsets {
        out.u16(o);
    }
    for g in glyphs {
        out.u16(g);
    }
    Ok(out)
}

/// A `cmap` with one format 4 subtable shared by the given (platform, encoding) records.
pub(crate) fn table(map: &BTreeMap<u32, u16>, records: &[(u16, u16)]) -> Result<Vec<u8>> {
    let sub = format4_table(map)?;
    let mut out = Vec::new();
    out.u16(0);
    out.u16(records.len() as u16);
    for &(platform, encoding) in records {
        out.u16(platform);
        out.u16(encoding);
        out.u32((4 + 8 * records.len()) as u32);
    }
    out.extend_from_slice(&sub);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format4_reads_back_what_it_writes() {
        let mut map = BTreeMap::new();
        for (c, g) in [
            (0x20, 3),
            (0x21, 4),
            (0x22, 9),
            (0x30, 5),
            (0x4e00, 7),
            (0x4e02, 8),
            (0xe0e0, 1),
        ] {
            map.insert(c, g);
        }
        let t = table(&map, &[(3, 1)]).unwrap();
        assert_eq!(read(Bytes(&t)).unwrap(), map);
    }
}
