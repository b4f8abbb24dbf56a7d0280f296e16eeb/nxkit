//! Outline drawing with fontTools' pen semantics, so the icons get the points that NX2Apple's `make_ext_font.py` gets:
//! a glyph drawn as segments (`Glyph.draw`), its bounds (`BoundsPen`), the reversal of a contour
//! (`reversedContour`) and the TrueType pen that collects points (`TTGlyphPen`).

use crate::glyph::Point;
use crate::sfnt::ot_round;
use crate::{Error, Result};

/// A point with float coordinates.
pub(crate) type P = (f64, f64);

/// One drawing command. `Quad` holds the off-curve points and, last, the on-curve point.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Op {
    Move(P),
    Line(P),
    Quad(Vec<P>),
    Close,
}

impl Op {
    fn points(&self) -> &[P] {
        match self {
            Op::Move(p) | Op::Line(p) => std::slice::from_ref(p),
            Op::Quad(v) => v,
            Op::Close => &[],
        }
    }
}

/// The commands of a simple glyph's contours as `Glyph.draw` gives them: each contour turns so that it ends on an
/// on-curve point, which is the move, and the closing line is left to the close.
pub(crate) fn draw(contours: &[Vec<Point>], dx: f64, out: &mut Vec<Op>) -> Result<()> {
    for c in contours {
        let first = c
            .iter()
            .position(|p| p.2)
            .ok_or_else(|| Error::new("a letter contour without an on-curve point"))?
            + 1;
        let mut rest: Vec<(P, bool)> = c[first..]
            .iter()
            .chain(&c[..first])
            .map(|p| ((f64::from(p.0) + dx, f64::from(p.1)), p.2))
            .collect();
        let Some(&(start, _)) = rest.last() else {
            continue;
        };
        out.push(Op::Move(start));
        while !rest.is_empty() {
            let next = rest.iter().position(|p| p.1).unwrap_or(rest.len() - 1) + 1;
            if next == 1 {
                if rest.len() > 1 {
                    out.push(Op::Line(rest[0].0));
                }
            } else {
                out.push(Op::Quad(rest[..next].iter().map(|p| p.0).collect()));
            }
            rest.drain(..next);
        }
        out.push(Op::Close);
    }
    Ok(())
}

/// Applies `x * s + dx`, `y * s + dy` to every point, in the order of fontTools' `Transform.transformPoint`.
pub(crate) fn transform(ops: &mut [Op], s: f64, dx: f64, dy: f64) {
    let t = |p: &mut P| *p = (s * p.0 + dx, s * p.1 + dy);
    for op in ops {
        match op {
            Op::Move(p) | Op::Line(p) => t(p),
            Op::Quad(v) => v.iter_mut().for_each(t),
            Op::Close => {}
        }
    }
}

fn grow(b: &mut Option<[f64; 4]>, p: P) {
    *b = Some(match *b {
        None => [p.0, p.1, p.0, p.1],
        Some(r) => [r[0].min(p.0), r[1].min(p.1), r[2].max(p.0), r[3].max(p.1)],
    });
}

/// The bounds of the outline with the extremes of its curves (`BoundsPen`).
pub(crate) fn bounds(ops: &[Op]) -> Option<[f64; 4]> {
    let mut b = None;
    let mut cur = (0.0, 0.0);
    for op in ops {
        match op {
            Op::Move(p) | Op::Line(p) => {
                grow(&mut b, *p);
                cur = *p;
            }
            Op::Quad(v) if v.len() <= 1 => {
                if let Some(&p) = v.first() {
                    grow(&mut b, p);
                    cur = p;
                }
            }
            Op::Quad(v) => {
                let n = v.len();
                for i in 0..n - 1 {
                    let pt = if i + 2 == n {
                        v[n - 1]
                    } else {
                        (0.5 * (v[i].0 + v[i + 1].0), 0.5 * (v[i].1 + v[i + 1].1))
                    };
                    let bcp = v[i];
                    grow(&mut b, pt);
                    if let Some(r) = b
                        && !(r[0] <= bcp.0 && bcp.0 <= r[2] && r[1] <= bcp.1 && bcp.1 <= r[3])
                    {
                        for q in quad_extremes(cur, bcp, pt) {
                            grow(&mut b, q);
                        }
                    }
                    cur = pt;
                }
            }
            Op::Close => {}
        }
    }
    b
}

/// The points that fontTools' `calcQuadraticBounds` bounds: the extremes inside the segment and its two ends.
fn quad_extremes(p1: P, p2: P, p3: P) -> Vec<P> {
    let (bx, by) = ((p2.0 - p1.0) * 2.0, (p2.1 - p1.1) * 2.0);
    let (ax, ay) = (p3.0 - p1.0 - bx, p3.1 - p1.1 - by);
    let mut roots = Vec::new();
    if ax * 2.0 != 0.0 {
        roots.push(-bx / (ax * 2.0));
    }
    if ay * 2.0 != 0.0 {
        roots.push(-by / (ay * 2.0));
    }
    let mut out: Vec<P> = roots
        .into_iter()
        .filter(|t| (0.0..1.0).contains(t))
        .map(|t| (ax * t * t + bx * t + p1.0, ay * t * t + by * t + p1.1))
        .collect();
    out.extend([p1, p3]);
    out
}

/// Twice the signed area of a ring as `make_ext_font.py`'s `area` sums it (positive for a clockwise ring).
pub(crate) fn area(pts: &[P]) -> f64 {
    let n = pts.len();
    (0..n)
        .map(|i| {
            let (c, p) = (pts[i], pts[(i + n - 1) % n]);
            c.0 * p.1 - p.0 * c.1
        })
        .sum::<f64>()
        / 2.0
}

/// The rings of an outline: each contour's points as the commands list them, off-curve points included.
pub(crate) fn rings(ops: &[Op]) -> Vec<Vec<P>> {
    let mut out: Vec<Vec<P>> = Vec::new();
    for op in ops {
        match op {
            Op::Move(p) => out.push(vec![*p]),
            Op::Line(_) | Op::Quad(_) => {
                if let Some(r) = out.last_mut() {
                    r.extend_from_slice(op.points());
                }
            }
            Op::Close => {}
        }
    }
    out
}

/// The outline with every contour in the other direction, as fontTools' `ReverseContourPen` gives it.
pub(crate) fn reversed(ops: &[Op]) -> Vec<Op> {
    let mut out = Vec::new();
    for contour in ops.split_inclusive(|op| *op == Op::Close) {
        reverse_one(contour, &mut out);
    }
    out
}

fn last(op: &Op) -> P {
    op.points().last().copied().unwrap_or_default()
}

fn with_last(op: &Op, p: P) -> Op {
    match op {
        Op::Quad(v) => {
            let mut v = v.clone();
            if let Some(l) = v.last_mut() {
                *l = p;
            }
            Op::Quad(v)
        }
        Op::Line(_) => Op::Line(p),
        Op::Move(_) => Op::Move(p),
        Op::Close => Op::Close,
    }
}

fn reverse_one(contour: &[Op], out: &mut Vec<Op>) {
    let Some((Op::Move(first), rest)) = contour.split_first() else {
        out.extend_from_slice(contour);
        return;
    };
    let mut segs: Vec<Op> = rest.iter().filter(|o| **o != Op::Close).cloned().collect();
    out.push(Op::Move(*first));
    let Some(last_op) = segs.last().cloned() else {
        out.push(Op::Close);
        return;
    };
    let last_on = last(&last_op);
    if *first != last_on {
        out.push(Op::Line(last_on));
        let n = segs.len();
        segs[n - 1] = with_last(&last_op, *first);
    }
    let second = if segs.len() > 1 {
        segs[0].clone()
    } else {
        last_op.clone()
    };
    if let Op::Line(p) = second
        && p != *first
    {
        segs.remove(0);
        if let Some(l) = segs.last_mut() {
            *l = with_last(&last_op, p);
        }
    }
    let n = segs.len();
    for i in (0..n).rev() {
        let next = last(&segs[(i + n - 1) % n]);
        out.push(match &segs[i] {
            Op::Quad(v) => {
                let mut r: Vec<P> = v[..v.len() - 1].iter().rev().copied().collect();
                r.push(next);
                Op::Quad(r)
            }
            other => with_last(other, next),
        });
    }
    out.push(Op::Close);
}

/// fontTools' `TTGlyphPen`: points and contour ends, a closing point that repeats the start dropped.
#[derive(Default)]
pub(crate) struct TtPen {
    points: Vec<(P, bool)>,
    ends: Vec<usize>,
}

impl TtPen {
    pub(crate) fn play(&mut self, ops: &[Op]) {
        for op in ops {
            match op {
                Op::Move(p) | Op::Line(p) => self.points.push((*p, true)),
                Op::Quad(v) => {
                    let n = v.len();
                    for (i, p) in v.iter().enumerate() {
                        self.points.push((*p, i + 1 == n));
                    }
                }
                Op::Close => self.close(),
            }
        }
    }

    fn close(&mut self) {
        let start = self.ends.last().map_or(0, |e| e + 1);
        if self.points.len() <= start + 1 {
            self.points.truncate(start);
            return;
        }
        let end = self.points.len() - 1;
        if self.points[start].0 == self.points[end].0 {
            self.points.pop();
        }
        self.ends.push(self.points.len() - 1);
    }

    /// The contours with the coordinates rounded by `otRound`.
    pub(crate) fn contours(&self) -> Vec<Vec<Point>> {
        let mut out = Vec::new();
        let mut start = 0;
        for &e in &self.ends {
            out.push(
                self.points[start..=e]
                    .iter()
                    .map(|&((x, y), on)| (ot_round(x), ot_round(y), on))
                    .collect(),
            );
            start = e + 1;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ops(v: &[(&str, &[P])]) -> Vec<Op> {
        v.iter()
            .map(|(k, p)| match *k {
                "M" => Op::Move(p[0]),
                "L" => Op::Line(p[0]),
                "Q" => Op::Quad(p.to_vec()),
                _ => Op::Close,
            })
            .collect()
    }

    // The expected outputs are those of fontTools 4.63's reversedContour for the same input.
    #[test]
    fn reversal_matches_fonttools() {
        let a = ops(&[
            ("M", &[(0., 0.)]),
            ("L", &[(100., 0.)]),
            ("Q", &[(150., 50.), (100., 100.)]),
            ("L", &[(0., 100.)]),
            ("Z", &[]),
        ]);
        let want = ops(&[
            ("M", &[(0., 0.)]),
            ("L", &[(0., 100.)]),
            ("L", &[(100., 100.)]),
            ("Q", &[(150., 50.), (100., 0.)]),
            ("Z", &[]),
        ]);
        assert_eq!(reversed(&a), want);
        let b = ops(&[
            ("M", &[(0., 0.)]),
            ("L", &[(100., 0.)]),
            ("Q", &[(150., 50.), (100., 100.)]),
            ("Q", &[(50., 150.), (-50., 50.), (0., 0.)]),
            ("Z", &[]),
        ]);
        let want = ops(&[
            ("M", &[(0., 0.)]),
            ("Q", &[(-50., 50.), (50., 150.), (100., 100.)]),
            ("Q", &[(150., 50.), (100., 0.)]),
            ("Z", &[]),
        ]);
        assert_eq!(reversed(&b), want);
        let c = ops(&[
            ("M", &[(0., 0.)]),
            ("Q", &[(50., -50.), (100., 0.)]),
            ("L", &[(50., 80.)]),
            ("Z", &[]),
        ]);
        let want = ops(&[
            ("M", &[(0., 0.)]),
            ("L", &[(50., 80.)]),
            ("L", &[(100., 0.)]),
            ("Q", &[(50., -50.), (0., 0.)]),
            ("Z", &[]),
        ]);
        assert_eq!(reversed(&c), want);
    }

    #[test]
    fn the_pen_drops_a_closing_point_that_repeats_the_start() {
        let mut pen = TtPen::default();
        pen.play(&ops(&[
            ("M", &[(0., 0.)]),
            ("L", &[(10., 0.)]),
            ("Q", &[(10., 10.), (0.4, 0.)]),
            ("Z", &[]),
        ]));
        pen.play(&ops(&[
            ("M", &[(0., 0.)]),
            ("Q", &[(5., 5.), (0., 0.)]),
            ("Z", &[]),
        ]));
        assert_eq!(
            pen.contours(),
            vec![
                vec![(0, 0, true), (10, 0, true), (10, 10, false), (0, 0, true)],
                vec![(0, 0, true), (5, 5, false)]
            ]
        );
    }
}
