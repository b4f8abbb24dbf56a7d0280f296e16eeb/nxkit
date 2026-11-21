//! The cut of types 0 to 4: the glyphs of a type's characters from one source font, without hinting, as NX2Apple's
//! `make_shared_fonts.py` makes them with fontTools' subsetter.
//!
//! A game sizes the work memory of each `nn::fontll` engine for Nintendo's font of that type. The memory that loading
//! a font needs grows with its hinting program and its outlines, so the cut drops the hinting (`fpgm`, `prep`, `cvt `,
//! `hdmx` and the instructions of each glyph) and holds the characters of its type only. Of the layout tables the
//! script keeps kerning only, and Arial Unicode has none, so `GSUB` and `GPOS` stay as empty tables. Each cut gets a
//! name of its own, as a game can load several types into one engine.

use crate::charsets::Data;
use crate::glyph::{Glyphs, Shape};
use crate::sfnt::{self, Bytes, Font, Name, Put};
use crate::{Error, Result, cmap};
use std::collections::{BTreeMap, BTreeSet};

/// The code points that the cut maps: the characters of the set that the font has, and their bidi mirrors
/// (fontTools' `bidi_closure`).
fn requested(
    set: &[(u32, u32)],
    map: &BTreeMap<u32, u16>,
    data: &Data,
) -> (usize, BTreeMap<u32, u16>) {
    let have: Vec<u32> = set
        .iter()
        .flat_map(|&(a, b)| map.range(a..=b).map(|(&c, _)| c))
        .collect();
    let mut out: BTreeMap<u32, u16> = have.iter().map(|c| (*c, map[c])).collect();
    for c in &have {
        if let Some(m) = data.mirrors.get(c)
            && let Some(&g) = map.get(m)
        {
            out.insert(*m, g);
        }
    }
    (have.len(), out)
}

/// Counts of a glyph after its components are resolved: points, contours and depth.
fn flat(
    glyphs: &Glyphs<'_>,
    gid: u16,
    depth: u16,
    memo: &mut BTreeMap<(u16, u16), (usize, usize, u16)>,
) -> Result<(usize, usize, u16)> {
    if depth > 16 {
        return Err(Error::new("composite glyphs nest too deep"));
    }
    if let Some(v) = memo.get(&(gid, depth)) {
        return Ok(*v);
    }
    let v = match glyphs.shape(gid)? {
        Shape::Empty => (0, 0, depth),
        Shape::Simple(p, c) => (p, c, depth),
        Shape::Composite(parts) => {
            let (mut p, mut c, mut d) = (0, 0, depth);
            for part in parts {
                match glyphs.shape(part)? {
                    Shape::Empty => {}
                    Shape::Simple(pp, cc) => {
                        p += pp;
                        c += cc;
                    }
                    Shape::Composite(_) => {
                        let (pp, cc, dd) = flat(glyphs, part, depth + 1, memo)?;
                        p += pp;
                        c += cc;
                        d = d.max(dd);
                    }
                }
            }
            (p, c, d)
        }
    };
    memo.insert((gid, depth), v);
    Ok(v)
}

/// The cut of `source` for the code point ranges `set`, named `name`. Returns the font and its count of characters.
pub(crate) fn cut(
    source: &[u8],
    set: &[(u32, u32)],
    name: &str,
    data: &Data,
) -> Result<(Vec<u8>, usize)> {
    let font = Font::parse(source)?;
    let head = font.need(b"head")?;
    let maxp = font.need(b"maxp")?;
    let hhea = font.need(b"hhea")?;
    let count = usize::from(maxp.u16(4)?);
    let glyphs = Glyphs::new(
        font.need(b"glyf")?,
        font.need(b"loca")?,
        head.i16(50)? != 0,
        count,
    )?;
    let map = cmap::read(font.need(b"cmap")?)?;
    let (chars, cmapped) = requested(set, &map, data);

    let mut keep: BTreeSet<u16> = cmapped.values().copied().collect();
    keep.insert(0);
    let mut todo: Vec<u16> = keep.iter().copied().collect();
    while let Some(g) = todo.pop() {
        if let Shape::Composite(parts) = glyphs.shape(g)? {
            for p in parts {
                if usize::from(p) >= count {
                    return Err(Error::new(format!(
                        "glyph {g} has a component {p} out of range"
                    )));
                }
                if keep.insert(p) {
                    todo.push(p);
                }
            }
        }
    }
    let order: Vec<u16> = keep.into_iter().collect();
    let new_id: BTreeMap<u16, u16> = order
        .iter()
        .enumerate()
        .map(|(i, &g)| (g, i as u16))
        .collect();
    let remap = |g: u16| {
        new_id
            .get(&g)
            .copied()
            .ok_or_else(|| Error::new(format!("glyph {g} is not in the cut")))
    };

    let new_map: BTreeMap<u32, u16> = cmapped
        .iter()
        .map(|(&c, &g)| Ok((c, remap(g)?)))
        .collect::<Result<_>>()?;
    let data_of = order
        .iter()
        .map(|&g| crate::glyph::unhinted(glyphs.data(g)?, remap));
    let (glyf, loca_t, long) = crate::glyph::tables(data_of.collect::<Result<_>>()?);

    let hmtx_src = font.need(b"hmtx")?;
    let full = usize::from(hhea.u16(34)?).max(1);
    let metric = |g: u16| -> Result<(u16, i16)> {
        let g = usize::from(g);
        if g < full {
            Ok((hmtx_src.u16(4 * g)?, hmtx_src.i16(4 * g + 2)?))
        } else {
            Ok((
                hmtx_src.u16(4 * (full - 1))?,
                hmtx_src.i16(4 * full + 2 * (g - full))?,
            ))
        }
    };
    let metrics: Vec<(u16, i16)> = order.iter().map(|&g| metric(g)).collect::<Result<_>>()?;
    let (hmtx, long_metrics) = sfnt::hmtx(&metrics);

    let mut bbox: Option<[i16; 4]> = None;
    let (mut min_lsb, mut min_rsb, mut extent) = (i32::MAX, i32::MAX, i32::MIN);
    let (mut points, mut contours, mut c_points, mut c_contours, mut elements, mut depth) =
        (0, 0, 0, 0, 0, 0u16);
    let mut memo = BTreeMap::new();
    for (i, &g) in order.iter().enumerate() {
        let shape = glyphs.shape(g)?;
        if let Some(b) = glyphs.bounds(g)?
            && !matches!(shape, Shape::Simple(_, 0))
        {
            bbox = Some(match bbox {
                None => b,
                Some(a) => [
                    a[0].min(b[0]),
                    a[1].min(b[1]),
                    a[2].max(b[2]),
                    a[3].max(b[3]),
                ],
            });
            let (adv, lsb) = metrics[i];
            let width = i32::from(b[2]) - i32::from(b[0]);
            min_lsb = min_lsb.min(i32::from(lsb));
            min_rsb = min_rsb.min(i32::from(adv) - i32::from(lsb) - width);
            extent = extent.max(i32::from(lsb) + width);
        }
        match shape {
            Shape::Simple(p, c) => {
                points = points.max(p);
                contours = contours.max(c);
            }
            Shape::Composite(parts) => {
                let (p, c, d) = flat(&glyphs, g, 1, &mut memo)?;
                c_points = c_points.max(p);
                c_contours = c_contours.max(c);
                elements = elements.max(parts.len());
                depth = depth.max(d);
            }
            Shape::Empty => {}
        }
    }
    let bbox = bbox.unwrap_or_default();

    let mut head_t = head.slice(0, 54)?.to_vec();
    for (k, v) in bbox.iter().enumerate() {
        sfnt::set_u16(&mut head_t, 36 + 2 * k, *v as u16);
    }
    sfnt::set_u16(&mut head_t, 50, u16::from(long));

    let mut maxp_t = maxp.slice(0, 32)?.to_vec();
    let clamp = |v: usize| v.min(0xffff) as u16;
    for (at, v) in [
        (4, order.len() as u16),
        (6, clamp(points)),
        (8, clamp(contours)),
        (10, clamp(c_points)),
        (12, clamp(c_contours)),
        (14, 1),
        (16, 0),
        (18, 0),
        (20, 0),
        (22, 0),
        (24, 0),
        (26, 0),
        (28, clamp(elements)),
        (30, depth),
    ] {
        sfnt::set_u16(&mut maxp_t, at, v);
    }

    let mut hhea_t = hhea.slice(0, 36)?.to_vec();
    let adv_max = metrics.iter().map(|m| m.0).max().unwrap_or(0);
    let none = min_lsb == i32::MAX;
    for (at, v) in [
        (10, adv_max as i32),
        (12, if none { 0 } else { min_lsb }),
        (14, if none { 0 } else { min_rsb }),
        (16, if none { 0 } else { extent }),
        (34, i32::from(long_metrics)),
    ] {
        sfnt::set_u16(&mut hhea_t, at, v as u16);
    }

    let mut tables = vec![
        (*b"head", head_t),
        (*b"hhea", hhea_t),
        (*b"maxp", maxp_t),
        (*b"hmtx", hmtx),
        (*b"cmap", cmap::table(&new_map, &[(3, 1)])?),
        (*b"loca", loca_t),
        (*b"glyf", glyf),
        (*b"name", names(font.need(b"name")?, name)?),
        (
            *b"post",
            sfnt::post(
                font.need(b"post")?
                    .slice(0, 32)?
                    .try_into()
                    .unwrap_or([0; 32]),
                None,
            ),
        ),
    ];
    if let Some(os2) = font.table(b"OS/2") {
        tables.push((*b"OS/2", os2_table(os2, &new_map, data)?));
    }
    if let Some(gasp) = font.table(b"gasp") {
        tables.push((*b"gasp", gasp.0.to_vec()));
    }
    for tag in [b"GSUB", b"GPOS"] {
        if font.table(tag).is_some() {
            let mut empty = Vec::new();
            for v in [1u16, 0, 10, 12, 14, 0, 0, 0] {
                empty.u16(v);
            }
            tables.push((*tag, empty));
        }
    }
    Ok((sfnt::assemble(tables), chars))
}

/// OS/2 with the first and last character of the cut, and without the Unicode range bits of blocks it does not hold.
/// The code page bits stay as in the source (fontTools clears the bits of the code pages a cut lacks).
fn os2_table(os2: Bytes<'_>, map: &BTreeMap<u32, u16>, data: &Data) -> Result<Vec<u8>> {
    let mut t = os2.0.to_vec();
    if t.len() < 68 {
        return Err(Error::new("OS/2 is too short"));
    }
    let used = data.unicode_ranges(map.keys().copied());
    for k in 0..4 {
        let at = 42 + 4 * k;
        let old = os2.u32(at)?;
        let keep = old & (used >> (32 * k)) as u32;
        t[at..at + 4].copy_from_slice(&keep.to_be_bytes());
    }
    if let (Some(&first), Some(&last)) = (map.keys().next(), map.keys().next_back()) {
        sfnt::set_u16(&mut t, 64, first.min(0xffff) as u16);
        sfnt::set_u16(&mut t, 66, last.min(0xffff) as u16);
    }
    Ok(t)
}

/// The Unicode and Windows records of the source `name` table, with the family, full and typographic family names set
/// to `family` and the unique and PostScript names to `family` with hyphens.
fn names(src: Bytes<'_>, family: &str) -> Result<Vec<u8>> {
    let count = usize::from(src.u16(2)?);
    let storage = usize::from(src.u16(4)?);
    let ps = family.replace(' ', "-");
    let mut out = Vec::new();
    for i in 0..count {
        let r = 6 + 12 * i;
        let (platform, encoding, language, id) = (
            src.u16(r)?,
            src.u16(r + 2)?,
            src.u16(r + 4)?,
            src.u16(r + 6)?,
        );
        let unicode = platform == 0 || (platform == 3 && matches!(encoding, 0 | 1 | 10));
        if !unicode || language >= 0x8000 {
            continue;
        }
        let string = match id {
            1 | 4 | 16 => sfnt::utf16(family),
            3 | 6 => sfnt::utf16(&ps),
            _ => src
                .slice(
                    storage + usize::from(src.u16(r + 10)?),
                    usize::from(src.u16(r + 8)?),
                )?
                .to_vec(),
        };
        out.push(Name {
            platform,
            encoding,
            language,
            id,
            string,
        });
    }
    Ok(sfnt::name(out))
}
