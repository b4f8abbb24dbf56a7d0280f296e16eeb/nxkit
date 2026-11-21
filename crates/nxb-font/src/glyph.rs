//! `glyf` glyphs: the reading of simple and composite glyphs, the cut of their instructions, and the writing of a
//! simple glyph from its points.

use crate::sfnt::{Bytes, Put};
use crate::{Error, Result};

const ON_CURVE: u8 = 0x01;
const X_SHORT: u8 = 0x02;
const Y_SHORT: u8 = 0x04;
const REPEAT: u8 = 0x08;
const X_SAME: u8 = 0x10;
const Y_SAME: u8 = 0x20;

const ARG_WORDS: u16 = 0x0001;
const HAVE_SCALE: u16 = 0x0008;
const MORE: u16 = 0x0020;
const XY_SCALE: u16 = 0x0040;
const TWO_BY_TWO: u16 = 0x0080;
const HAVE_INSTRUCTIONS: u16 = 0x0100;

/// The glyph data of a font: `glyf` with the offsets of `loca`.
pub(crate) struct Glyphs<'a> {
    glyf: Bytes<'a>,
    loca: Vec<usize>,
}

/// What a glyph is made of.
pub(crate) enum Shape {
    Empty,
    /// Points and contours.
    Simple(usize, usize),
    /// The glyph IDs of the components.
    Composite(Vec<u16>),
}

impl<'a> Glyphs<'a> {
    pub(crate) fn new(glyf: Bytes<'a>, loca: Bytes<'_>, long: bool, count: usize) -> Result<Self> {
        let mut offs = Vec::with_capacity(count + 1);
        for i in 0..=count {
            offs.push(if long {
                loca.u32(4 * i)? as usize
            } else {
                2 * loca.u16(2 * i)? as usize
            });
        }
        if offs.windows(2).any(|w| w[1] < w[0]) || offs.last().is_some_and(|&e| e > glyf.0.len()) {
            return Err(Error::new("bad loca offsets"));
        }
        Ok(Glyphs { glyf, loca: offs })
    }

    pub(crate) fn data(&self, gid: u16) -> Result<&'a [u8]> {
        let g = usize::from(gid);
        match (self.loca.get(g), self.loca.get(g + 1)) {
            (Some(&a), Some(&b)) => self.glyf.slice(a, b - a),
            _ => Err(Error::new(format!("glyph {gid} is out of range"))),
        }
    }

    pub(crate) fn shape(&self, gid: u16) -> Result<Shape> {
        let d = Bytes(self.data(gid)?);
        if d.0.is_empty() {
            return Ok(Shape::Empty);
        }
        let contours = d.i16(0)?;
        if contours >= 0 {
            let n = contours as usize;
            let points = if n == 0 {
                0
            } else {
                usize::from(d.u16(10 + 2 * (n - 1))?) + 1
            };
            return Ok(Shape::Simple(points, n));
        }
        Ok(Shape::Composite(
            components(d)?.0.into_iter().map(|c| c.1).collect(),
        ))
    }

    /// The (xMin, yMin, xMax, yMax) of the glyph header, `None` for an empty glyph.
    pub(crate) fn bounds(&self, gid: u16) -> Result<Option<[i16; 4]>> {
        let d = Bytes(self.data(gid)?);
        if d.0.is_empty() {
            return Ok(None);
        }
        Ok(Some([d.i16(2)?, d.i16(4)?, d.i16(6)?, d.i16(8)?]))
    }
}

/// A component record of a composite glyph: its offset, glyph ID and flags.
type Component = (usize, u16, u16);

/// The components of a composite glyph and the end of the records.
fn components(d: Bytes<'_>) -> Result<(Vec<Component>, usize)> {
    let mut out = Vec::new();
    let mut at = 10;
    loop {
        let flags = d.u16(at)?;
        out.push((at, d.u16(at + 2)?, flags));
        at += 4 + if flags & ARG_WORDS != 0 { 4 } else { 2 };
        at += if flags & HAVE_SCALE != 0 {
            2
        } else if flags & XY_SCALE != 0 {
            4
        } else if flags & TWO_BY_TWO != 0 {
            8
        } else {
            0
        };
        d.slice(at, 0)?;
        if flags & MORE == 0 {
            return Ok((out, at));
        }
    }
}

/// The glyph without its instructions and with the glyph IDs of its components mapped. A simple glyph is written
/// again from its points, so its bounds come from the points, as fontTools recompiles it.
pub(crate) fn unhinted(data: &[u8], map: impl Fn(u16) -> Result<u16>) -> Result<Vec<u8>> {
    let d = Bytes(data);
    if data.is_empty() {
        return Ok(Vec::new());
    }
    if d.i16(0)? >= 0 {
        return simple(&points(data)?);
    }
    let (comps, end) = components(d)?;
    let mut out = d.slice(0, end)?.to_vec();
    for (at, gid, flags) in comps {
        crate::sfnt::set_u16(&mut out, at, flags & !HAVE_INSTRUCTIONS);
        crate::sfnt::set_u16(&mut out, at + 2, map(gid)?);
    }
    Ok(out)
}

/// `glyf` and `loca` from the glyphs, as fontTools compiles them: no padding, but a byte after each glyph of odd
/// length when that lets `loca` use short offsets. Returns glyf, loca and whether the offsets are long.
pub(crate) fn tables(mut glyphs: Vec<Vec<u8>>) -> (Vec<u8>, Vec<u8>, bool) {
    let total: usize = glyphs.iter().map(Vec::len).sum();
    let odd = glyphs.iter().filter(|g| g.len() % 2 == 1).count();
    if total < 0x20000 && odd > 0 && total + odd < 0x20000 {
        for g in glyphs.iter_mut().filter(|g| g.len() % 2 == 1) {
            g.push(0);
        }
    }
    let mut offs = vec![0usize];
    for g in &glyphs {
        offs.push(offs[offs.len() - 1] + g.len());
    }
    let long = offs.iter().any(|&o| o % 2 == 1 || o >= 0x20000);
    let mut loca = Vec::new();
    for o in offs {
        if long {
            loca.u32(o as u32);
        } else {
            loca.u16((o / 2) as u16);
        }
    }
    (glyphs.concat(), loca, long)
}

/// The flags of every point (repeats expanded) and the offset after them.
fn flags(d: Bytes<'_>, contours: usize, mut at: usize) -> Result<(Vec<u8>, usize)> {
    let points = if contours == 0 {
        0
    } else {
        usize::from(d.u16(10 + 2 * (contours - 1))?) + 1
    };
    let mut out = Vec::with_capacity(points);
    while out.len() < points {
        let f = d.u8(at)?;
        at += 1;
        out.push(f);
        if f & REPEAT != 0 {
            let n = d.u8(at)?;
            at += 1;
            for _ in 0..n {
                out.push(f);
            }
        }
    }
    out.truncate(points);
    Ok((out, at))
}

/// A point of an outline: x, y and whether it is on the curve.
pub(crate) type Point = (i32, i32, bool);

/// The contours of a simple glyph as points.
pub(crate) fn points(data: &[u8]) -> Result<Vec<Vec<Point>>> {
    let d = Bytes(data);
    if data.is_empty() {
        return Ok(Vec::new());
    }
    let contours = d.i16(0)?;
    if contours < 0 {
        return Err(Error::new("a composite glyph where a simple one is needed"));
    }
    let n = contours as usize;
    let ins = 10 + 2 * n;
    let (flags, mut at) = flags(d, n, ins + 2 + usize::from(d.u16(ins)?))?;
    let mut coord = |short: u8, same: u8| -> Result<Vec<i32>> {
        let mut v = 0i32;
        let mut out = Vec::with_capacity(flags.len());
        for f in &flags {
            if f & short != 0 {
                let b = i32::from(d.u8(at)?);
                at += 1;
                v += if f & same != 0 { b } else { -b };
            } else if f & same == 0 {
                v += i32::from(d.i16(at)?);
                at += 2;
            }
            out.push(v);
        }
        Ok(out)
    };
    let xs = coord(X_SHORT, X_SAME)?;
    let ys = coord(Y_SHORT, Y_SAME)?;
    let mut out = Vec::with_capacity(n);
    let mut start = 0;
    for c in 0..n {
        let end = usize::from(d.u16(10 + 2 * c)?) + 1;
        if end <= start || end > flags.len() {
            return Err(Error::new("bad contour ends"));
        }
        out.push(
            (start..end)
                .map(|i| (xs[i], ys[i], flags[i] & ON_CURVE != 0))
                .collect(),
        );
        start = end;
    }
    Ok(out)
}

/// A simple glyph from its contours, with its bounds from the points and no instructions.
pub(crate) fn simple(contours: &[Vec<Point>]) -> Result<Vec<u8>> {
    let all: Vec<&Point> = contours.iter().flatten().collect();
    if all.is_empty() {
        return Ok(Vec::new());
    }
    let fit =
        |v: i32| i16::try_from(v).map_err(|_| Error::new(format!("coordinate {v} out of range")));
    let min_x = all.iter().map(|p| p.0).min().unwrap_or(0);
    let min_y = all.iter().map(|p| p.1).min().unwrap_or(0);
    let max_x = all.iter().map(|p| p.0).max().unwrap_or(0);
    let max_y = all.iter().map(|p| p.1).max().unwrap_or(0);
    let mut out = Vec::new();
    out.i16(contours.len() as i16);
    for v in [min_x, min_y, max_x, max_y] {
        out.i16(fit(v)?);
    }
    let mut end = 0usize;
    for c in contours {
        end += c.len();
        out.u16((end - 1) as u16);
    }
    out.u16(0);
    let (mut flags, mut xs, mut ys) = (Vec::new(), Vec::new(), Vec::new());
    let (mut px, mut py) = (0, 0);
    for p in &all {
        let mut f = if p.2 { ON_CURVE } else { 0 };
        for (v, last, short, same, bytes) in [
            (p.0, &mut px, X_SHORT, X_SAME, &mut xs),
            (p.1, &mut py, Y_SHORT, Y_SAME, &mut ys),
        ] {
            let delta = v - *last;
            *last = v;
            if delta == 0 {
                f |= same;
            } else if (-255..=255).contains(&delta) {
                f |= short;
                if delta > 0 {
                    f |= same;
                }
                bytes.push(delta.unsigned_abs() as u8);
            } else {
                bytes.extend_from_slice(&fit(delta)?.to_be_bytes());
            }
        }
        flags.push(f);
    }
    let mut i = 0;
    while i < flags.len() {
        let f = flags[i];
        let mut run = 0;
        while i + 1 + run < flags.len() && flags[i + 1 + run] == f && run < 255 {
            run += 1;
        }
        if run > 1 {
            out.push(f | REPEAT);
            out.push(run as u8);
        } else {
            out.push(f);
            run = 0;
        }
        i += 1 + run;
    }
    out.extend_from_slice(&xs);
    out.extend_from_slice(&ys);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_simple_glyph_reads_back() {
        let contours = vec![
            vec![
                (950, 350, true),
                (950, 536, false),
                (500, 800, true),
                (-400, 800, true),
                (-400, 800, true),
            ],
            vec![(0, 0, true), (300, 0, true), (300, 300, false)],
        ];
        let g = simple(&contours).unwrap();
        assert_eq!(points(&g).unwrap(), contours);
        assert_eq!(&g[2..10], &[0xfe, 0x70, 0, 0, 3, 0xb6, 3, 0x20]);
    }

    #[test]
    fn unhinted_drops_the_instructions() {
        let mut g = vec![0, 1, 0, 0, 0, 0, 0, 10, 0, 10, 0, 1, 0, 3, 0xb0, 0, 0];
        g.extend_from_slice(&[
            ON_CURVE | X_SAME | Y_SAME,
            ON_CURVE | X_SHORT | X_SAME | Y_SHORT | Y_SAME,
            10,
            10,
        ]);
        g.extend_from_slice(&[0, 0, 0]);
        let u = unhinted(&g, Ok).unwrap();
        assert_eq!(&u[12..14], &[0, 0]);
        assert_eq!(
            points(&u).unwrap(),
            vec![vec![(0, 0, true), (10, 10, true)]]
        );
        assert_eq!(u.len(), 18);
    }
}
