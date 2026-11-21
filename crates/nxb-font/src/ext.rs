//! Type 5, the Nintendo extension font: controller button icons at the private-use code points of the console's
//! font, as NX2Apple's `make_ext_font.py` draws them. The letters come from a letter font (macOS's Arial Rounded
//! Bold) and are knocked out of a solid plate. Every other code point of U+E000 to U+E1FF gets a plain ring, because a
//! game can take one of these glyphs without a null check.

use crate::glyph::{self, Glyphs, Point};
use crate::pen::{self, Op, P, TtPen};
use crate::sfnt::{self, Font, Name, Put, ot_round};
use crate::{Error, Result, charsets::Data, cmap};
use std::collections::BTreeMap;
use std::f64::consts::PI;

const EM: u16 = 1000;
const ASC: i16 = 880;
const DESC: i16 = -120;
/// The centre height and the radius of an icon: -100 to 800, on the baseline.
const CY: f64 = 350.0;
const R: f64 = 450.0;

type Pt = (f64, f64, bool);

/// Python's `round` of a float: halves go to the even neighbour.
fn round(v: f64) -> f64 {
    v.round_ties_even()
}

/// A contour of the plate: solid or hole by the sign of its area, started on its first on-curve point.
fn contour(pen: &mut TtPen, pts: &[Pt], hole: bool) {
    let ring: Vec<P> = pts.iter().map(|p| (p.0, p.1)).collect();
    let mut pts = pts.to_vec();
    if (pen::area(&ring) > 0.0) != hole {
        pts.reverse();
    }
    let Some(k) = pts.iter().position(|p| p.2) else {
        return;
    };
    pts.rotate_left(k);
    let mut ops = vec![Op::Move((pts[0].0, pts[0].1))];
    let mut offs: Vec<P> = Vec::new();
    for &(x, y, on) in &pts[1..] {
        if !on {
            offs.push((x, y));
        } else if offs.is_empty() {
            ops.push(Op::Line((x, y)));
        } else {
            offs.push((x, y));
            ops.push(Op::Quad(std::mem::take(&mut offs)));
        }
    }
    if !offs.is_empty() {
        offs.push((pts[0].0, pts[0].1));
        ops.push(Op::Quad(offs));
    }
    ops.push(Op::Close);
    pen.play(&ops);
}

fn circle(cx: f64, cy: f64, r: f64) -> Vec<Pt> {
    let n = 8.0;
    let mut pts = Vec::new();
    for i in 0..8 {
        let a = 2.0 * PI * f64::from(i) / n;
        let b = a + PI / n;
        let rr = r / (PI / n).cos();
        pts.push((round(cx + r * a.cos()), round(cy + r * a.sin()), true));
        pts.push((round(cx + rr * b.cos()), round(cy + rr * b.sin()), false));
    }
    pts
}

fn rrect(x0: f64, y0: f64, x1: f64, y1: f64, rad: f64) -> Vec<Pt> {
    let rad = rad.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0);
    vec![
        (x0 + rad, y1, true),
        (x1 - rad, y1, true),
        (x1, y1, false),
        (x1, y1 - rad, true),
        (x1, y0 + rad, true),
        (x1, y0, false),
        (x1 - rad, y0, true),
        (x0 + rad, y0, true),
        (x0, y0, false),
        (x0, y0 + rad, true),
        (x0, y1 - rad, true),
        (x0, y1, false),
    ]
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Pt> {
    vec![
        (x0, y1, true),
        (x1, y1, true),
        (x1, y0, true),
        (x0, y0, true),
    ]
}

fn rot(pts: &[Pt], cx: f64, cy: f64, deg: f64) -> Vec<Pt> {
    let a = deg.to_radians();
    let (c, s) = (a.cos(), a.sin());
    pts.iter()
        .map(|&(x, y, on)| {
            (
                round(cx + (x - cx) * c - (y - cy) * s),
                round(cy + (x - cx) * s + (y - cy) * c),
                on,
            )
        })
        .collect()
}

/// The outline of the D-pad: one closed ring.
fn cross(cx: f64, cy: f64, half: f64, arm: f64) -> Vec<Pt> {
    let (a, h) = (arm / 2.0, half);
    [
        (-a, h),
        (a, h),
        (a, a),
        (h, a),
        (h, -a),
        (a, -a),
        (a, -h),
        (-a, -h),
        (-a, -a),
        (-h, -a),
        (-h, a),
        (-a, a),
    ]
    .iter()
    .map(|&(x, y)| (cx + x, cy + y, true))
    .collect()
}

/// The letter font: its glyphs, advances, side bearings and Unicode map.
struct Letters<'a> {
    glyphs: Glyphs<'a>,
    hmtx: sfnt::Bytes<'a>,
    full: usize,
    map: BTreeMap<u32, u16>,
}

impl<'a> Letters<'a> {
    fn new(font: &Font<'a>) -> Result<Self> {
        let count = usize::from(font.need(b"maxp")?.u16(4)?);
        let long = font.need(b"head")?.i16(50)? != 0;
        Ok(Letters {
            glyphs: Glyphs::new(font.need(b"glyf")?, font.need(b"loca")?, long, count)?,
            hmtx: font.need(b"hmtx")?,
            full: usize::from(font.need(b"hhea")?.u16(34)?).max(1),
            map: cmap::read(font.need(b"cmap")?)?,
        })
    }

    fn metric(&self, g: u16) -> Result<(f64, f64)> {
        let g = usize::from(g);
        let (adv, lsb) = if g < self.full {
            (self.hmtx.u16(4 * g)?, self.hmtx.i16(4 * g + 2)?)
        } else {
            (
                self.hmtx.u16(4 * (self.full - 1))?,
                self.hmtx.i16(4 * self.full + 2 * (g - self.full))?,
            )
        };
        Ok((f64::from(adv), f64::from(lsb)))
    }

    /// The commands of glyph `g` moved right by `dx`, as fontTools' glyph set draws it (shifted by lsb - xMin).
    fn draw(&self, g: u16, dx: f64) -> Result<Vec<Op>> {
        let contours = glyph::points(self.glyphs.data(g)?)?;
        let shift = match self.glyphs.bounds(g)? {
            Some(b) => self.metric(g)?.1 - f64::from(b[0]),
            None => 0.0,
        };
        let mut ops = Vec::new();
        pen::draw(&contours, shift, &mut ops)?;
        pen::transform(&mut ops, 1.0, dx, 0.0);
        Ok(ops)
    }

    /// Knocks `text` out of the plate, centred at (cx, cy) with height `h`: the letters run against the plate.
    fn knock(&self, pen: &mut TtPen, text: &str, cx: f64, cy: f64, h: f64) -> Result<()> {
        let mut parts = Vec::new();
        let mut x = 0.0;
        for ch in text.chars() {
            let g = *self
                .map
                .get(&u32::from(ch))
                .ok_or_else(|| Error::new(format!("the letter font has no {ch:?}")))?;
            parts.push((g, x));
            x += self.metric(g)?.0;
        }
        let mut all = Vec::new();
        for &(g, dx) in &parts {
            all.extend(self.draw(g, dx)?);
        }
        let [x0, y0, x1, y1] =
            pen::bounds(&all).ok_or_else(|| Error::new(format!("{text:?} has no outline")))?;
        let s = h / (y1 - y0);
        let (ox, oy) = (cx - (x0 + x1) / 2.0 * s, cy - (y0 + y1) / 2.0 * s);
        let mut rec = Vec::new();
        for &(g, dx) in &parts {
            let mut ops = self.draw(g, 0.0)?;
            pen::transform(&mut ops, s, ox + dx * s, oy);
            rec.extend(ops);
        }
        let mut big: Option<f64> = None;
        for r in pen::rings(&rec) {
            let a = pen::area(&r);
            if big.is_none_or(|b| a.abs() > b.abs()) {
                big = Some(a);
            }
        }
        if big.is_some_and(|a| a < 0.0) {
            rec = pen::reversed(&rec);
        }
        pen.play(&rec);
        Ok(())
    }
}

/// How an icon is drawn.
#[derive(Clone, Copy)]
enum Draw {
    Round(&'static str),
    Plate(&'static str, f64),
    Sign(bool),
    Dpad(Option<f64>),
    Arrow(f64),
    Stick(Option<&'static str>),
    Ring,
}

fn paint(l: &Letters<'_>, pen: &mut TtPen, d: Draw, cx: f64) -> Result<()> {
    match d {
        Draw::Round(t) => {
            contour(pen, &circle(cx, CY, R), false);
            l.knock(pen, t, cx, CY, 470.0)?;
        }
        Draw::Plate(t, w) => {
            let h = 620.0;
            contour(
                pen,
                &rrect(
                    cx - w / 2.0,
                    CY - h / 2.0,
                    cx + w / 2.0,
                    CY + h / 2.0,
                    220.0,
                ),
                false,
            );
            l.knock(pen, t, cx, CY, 400.0)?;
        }
        Draw::Sign(minus) => {
            contour(pen, &circle(cx, CY, R), false);
            let (t, len) = (70.0, 250.0);
            if minus {
                contour(pen, &rect(cx - len, CY - t, cx + len, CY + t), true);
            } else {
                contour(pen, &cross(cx, CY, len, 2.0 * t), true);
            }
        }
        Draw::Dpad(arm) => {
            contour(pen, &cross(cx, CY, R, 320.0), false);
            if let Some(deg) = arm {
                contour(pen, &cross(cx, CY, R - 55.0, 320.0 - 110.0), true);
                let arm = rect(cx - 105.0, CY + 105.0, cx + 105.0, CY + R - 55.0);
                contour(pen, &rot(&arm, cx, CY, deg), false);
            }
        }
        Draw::Arrow(deg) => {
            let p = [
                (-380.0, 105.0),
                (60.0, 105.0),
                (60.0, 310.0),
                (420.0, 0.0),
                (60.0, -310.0),
                (60.0, -105.0),
                (-380.0, -105.0),
            ];
            let pts: Vec<Pt> = p.iter().map(|&(x, y)| (cx + x, CY + y, true)).collect();
            contour(pen, &rot(&pts, cx, CY, deg), false);
        }
        Draw::Stick(t) => {
            contour(pen, &circle(cx, CY, R), false);
            contour(pen, &circle(cx, CY, R - 70.0), true);
            contour(pen, &circle(cx, CY, 300.0), false);
            if let Some(t) = t {
                l.knock(pen, t, cx, CY, 300.0)?;
            }
        }
        Draw::Ring => {
            contour(pen, &circle(cx, CY, R), false);
            contour(pen, &circle(cx, CY, R - 70.0), true);
        }
    }
    Ok(())
}

/// The icons: glyph name, code points, advance and drawing.
fn icons() -> Vec<(String, Vec<u32>, u16, Draw)> {
    let mut v = Vec::new();
    for (i, t) in ["A", "B", "X", "Y"].into_iter().enumerate() {
        v.push((
            format!("btn{t}"),
            vec![0xe0a0 + i as u32, 0xe0e0 + i as u32],
            1000,
            Draw::Round(t),
        ));
    }
    for (i, (t, w)) in [
        ("L", 900),
        ("R", 900),
        ("ZL", 1200),
        ("ZR", 1200),
        ("SL", 1200),
        ("SR", 1200),
    ]
    .into_iter()
    .enumerate()
    {
        v.push((
            format!("btn{t}"),
            vec![0xe0a4 + i as u32, 0xe0e4 + i as u32],
            w + 100,
            Draw::Plate(t, f64::from(w)),
        ));
    }
    v.push((
        "btnPlus".into(),
        vec![0xe0b5, 0xe0ef],
        1000,
        Draw::Sign(false),
    ));
    v.push((
        "btnMinus".into(),
        vec![0xe0b6, 0xe0f0],
        1000,
        Draw::Sign(true),
    ));
    v.push(("dpad".into(), vec![0xe0ea], 1000, Draw::Dpad(None)));
    for (c, dir, deg) in [
        (0xe0ab, "right", 270.0),
        (0xe0ac, "down", 180.0),
        (0xe0ad, "up", 0.0),
        (0xe0ae, "left", 90.0),
    ] {
        v.push((
            format!("dpad_{dir}"),
            vec![c, c + 0x40],
            1000,
            Draw::Dpad(Some(deg)),
        ));
    }
    let arrows = [
        ("right", 0.0),
        ("left", 180.0),
        ("up", 90.0),
        ("down", 270.0),
        ("rightup", 45.0),
        ("rightdown", 315.0),
        ("leftdown", 225.0),
        ("leftup", 135.0),
    ];
    for (i, (n, deg)) in arrows.into_iter().enumerate() {
        v.push((
            format!("arrow_{n}"),
            vec![0xe090 + i as u32],
            1000,
            Draw::Arrow(deg),
        ));
    }
    v.push(("stick".into(), vec![0xe100], 1000, Draw::Stick(None)));
    v.push((
        "stickL".into(),
        vec![0xe101, 0xe0c1],
        1000,
        Draw::Stick(Some("L")),
    ));
    v.push((
        "stickR".into(),
        vec![0xe102, 0xe0c2],
        1000,
        Draw::Stick(Some("R")),
    ));
    let taken: Vec<u32> = v.iter().flat_map(|i| i.1.clone()).collect();
    let rest = (0xe000..0xe200).filter(|c| !taken.contains(c)).collect();
    v.push(("icon".into(), rest, 1000, Draw::Ring));
    v
}

/// The extension font from the letter font `letters`, and its count of code points.
pub(crate) fn build(letters: &[u8], data: &Data) -> Result<(Vec<u8>, usize)> {
    let font = Font::parse(letters)?;
    let l = Letters::new(&font)?;
    let mut list = icons();
    list.sort_by(|a, b| a.0.cmp(&b.0));
    let mut names = vec![".notdef".to_string()];
    let mut outlines: Vec<Vec<Vec<Point>>> = vec![Vec::new()];
    let mut advances = vec![500u16];
    let mut map = BTreeMap::new();
    for (name, codes, adv, d) in &list {
        let mut pen = TtPen::default();
        paint(&l, &mut pen, *d, f64::from(*adv) / 2.0)?;
        for &c in codes {
            map.insert(c, names.len() as u16);
        }
        names.push(name.clone());
        outlines.push(pen.contours());
        advances.push(*adv);
    }

    let mut data_of = Vec::new();
    let mut metrics = Vec::new();
    let mut bbox: Option<[i32; 4]> = None;
    let (mut min_lsb, mut min_rsb, mut extent) = (i32::MAX, i32::MAX, i32::MIN);
    let (mut points, mut contours) = (0usize, 0usize);
    for (c, &adv) in outlines.iter().zip(&advances) {
        data_of.push(glyph::simple(c)?);
        let all: Vec<&Point> = c.iter().flatten().collect();
        let lsb = all.iter().map(|p| p.0).min().unwrap_or(0);
        metrics.push((adv, lsb as i16));
        if all.is_empty() {
            continue;
        }
        let b = [
            lsb,
            all.iter().map(|p| p.1).min().unwrap_or(0),
            all.iter().map(|p| p.0).max().unwrap_or(0),
            all.iter().map(|p| p.1).max().unwrap_or(0),
        ];
        bbox = Some(bbox.map_or(b, |a| {
            [
                a[0].min(b[0]),
                a[1].min(b[1]),
                a[2].max(b[2]),
                a[3].max(b[3]),
            ]
        }));
        min_lsb = min_lsb.min(lsb);
        min_rsb = min_rsb.min(i32::from(adv) - b[2]);
        extent = extent.max(b[2]);
        points = points.max(all.len());
        contours = contours.max(c.len());
    }
    let bbox = bbox.unwrap_or_default();
    let (glyf, loca_t, long) = glyph::tables(data_of);
    let (hmtx, long_metrics) = sfnt::hmtx(&metrics);
    let stamp = font.need(b"head")?.slice(20, 16)?;

    let mut head = Vec::new();
    head.u32(0x0001_0000);
    head.u32(0x0001_0000);
    head.u32(0);
    head.u32(0x5f0f_3cf5);
    head.u16(3);
    head.u16(EM);
    head.extend_from_slice(stamp);
    for v in bbox {
        head.i16(v as i16);
    }
    for v in [0u16, 3, 2, u16::from(long), 0] {
        head.u16(v);
    }

    let mut hhea = Vec::new();
    hhea.u32(0x0001_0000);
    for v in [ASC, DESC, 0] {
        hhea.i16(v);
    }
    hhea.u16(advances.iter().copied().max().unwrap_or(0));
    for v in [min_lsb, min_rsb, extent] {
        hhea.i16(v as i16);
    }
    for v in [1u16, 0, 0, 0, 0, 0, 0, 0, long_metrics] {
        hhea.u16(v);
    }

    let mut maxp = Vec::new();
    maxp.u32(0x0001_0000);
    for v in [
        names.len(),
        points,
        contours,
        0,
        0,
        2,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ] {
        maxp.u16(v as u16);
    }

    let widths: Vec<f64> = advances
        .iter()
        .filter(|&&a| a > 0)
        .map(|&a| f64::from(a))
        .collect();
    let avg = if widths.is_empty() {
        0
    } else {
        ot_round(widths.iter().sum::<f64>() / widths.len() as f64)
    };
    let ranges = data.unicode_ranges(map.keys().copied());
    let mut os2 = Vec::new();
    os2.u16(3);
    os2.i16(avg as i16);
    for v in [400u16, 5, 4] {
        os2.u16(v);
    }
    os2.extend_from_slice(&[0; 22 + 10]);
    for k in 0..4 {
        os2.u32((ranges >> (32 * k)) as u32);
    }
    os2.extend_from_slice(b"????");
    os2.u16(0);
    let first = map.keys().next().copied().unwrap_or(0);
    let last = map.keys().next_back().copied().unwrap_or(0);
    os2.u16(first.min(0xffff) as u16);
    os2.u16(last.min(0xffff) as u16);
    for v in [ASC, DESC, 0] {
        os2.i16(v);
    }
    os2.u16(ASC as u16);
    os2.u16((-DESC) as u16);
    os2.extend_from_slice(&[0; 8 + 4]);
    for v in [0u16, 32, 0] {
        os2.u16(v);
    }

    let strings = [
        (1, "DM Nintendo Extension Stand-in"),
        (2, "Regular"),
        (4, "DM Nintendo Extension"),
        (6, "DM-Nintendo-Extension"),
    ];
    let mut name = Vec::new();
    for (id, s) in strings {
        name.push(Name {
            platform: 1,
            encoding: 0,
            language: 0,
            id,
            string: s.as_bytes().to_vec(),
        });
        name.push(Name {
            platform: 3,
            encoding: 1,
            language: 0x409,
            id,
            string: sfnt::utf16(s),
        });
    }
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();

    let chars = map.len();
    Ok((
        sfnt::assemble(vec![
            (*b"head", head),
            (*b"hhea", hhea),
            (*b"maxp", maxp),
            (*b"OS/2", os2),
            (*b"hmtx", hmtx),
            (*b"cmap", cmap::table(&map, &[(0, 3), (3, 1)])?),
            (*b"loca", loca_t),
            (*b"glyf", glyf),
            (*b"name", sfnt::name(name)),
            (*b"post", sfnt::post([0; 32], Some(&refs))),
        ]),
        chars,
    ))
}
