//! Texture offsets that are not constants, and the sampling that Metal's 1D textures lack.
//!
//! Metal takes an offset only as a constant. A register offset (TEX with AOFFI) moves the coordinate instead. The
//! shift is the offset in texels over the size of the level that is sampled. A fetch adds the offset to its integer
//! coordinate. A 1D texture has no level, bias or gradient in Metal, so such a sample reads the default level.

use super::emit::{Em, R};
use super::inst_tex::vec_of;
use crate::ir::agg::AggType;
use crate::structured::ast::OpData;

/// The offset of a texture operation, one part for each dimension.
pub(super) struct Off {
    pub parts: Vec<String>,
    pub constant: bool,
}

impl Off {
    /// The offset as the `int` vector Metal takes.
    pub(super) fn vector(&self) -> String {
        vec_of("int", &self.parts)
    }
}

/// What the sampled level is when the size of a level is needed.
pub(super) enum Level<'a> {
    /// The base level.
    Base,
    /// The level a float expression names.
    Float(&'a str),
    /// The level an integer expression names.
    Int(&'a str),
    /// The level the hardware picks for the coordinate, with this sampler.
    Implicit(&'a str),
}

impl Level<'_> {
    fn expr(&self, tex: &str, coord: &str) -> String {
        match self {
            Level::Base => "0u".into(),
            Level::Float(l) => format!("uint(max({l}, 0.0f))"),
            Level::Int(l) => format!("uint(max({l}, 0))"),
            Level::Implicit(s) => format!("uint({tex}.calculate_clamped_lod({s}, {coord}))"),
        }
    }
}

impl Em<'_> {
    /// The next `dims` sources as one offset.
    pub(super) fn offset_of(&mut self, o: &OpData, i: &mut usize, dims: i32) -> R<Off> {
        let mut parts = Vec::new();
        let mut constant = true;
        for _ in 0..dims {
            let a = self.src(o, *i)?;
            constant &= self.constant(a).is_some();
            parts.push(self.tsrc(o, i, AggType::S32)?.s);
        }
        Ok(Off { parts, constant })
    }
}

/// The coordinate moved by an offset.
///
/// `lod` names the level whose size converts the texels to coordinates. A fetch takes `uint` coordinates.
pub(super) fn shifted(coord: &str, off: &Off, tex: &str, int_coords: bool, lod: &Level) -> String {
    let n = off.parts.len();
    let (u, i, f) = if n == 1 {
        ("uint".to_string(), "int".to_string(), "float".to_string())
    } else {
        (format!("uint{n}"), format!("int{n}"), format!("float{n}"))
    };
    if int_coords {
        return format!("{u}({i}({coord}) + {})", off.vector());
    }
    let l = lod.expr(tex, coord);
    let size: Vec<String> = ["width", "height", "depth"]
        .iter()
        .take(n)
        .map(|d| format!("{tex}.get_{d}({l})"))
        .collect();
    format!(
        "({coord} + {f}({}) / {f}({}))",
        off.vector(),
        size.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn off(p: &[&str], constant: bool) -> Off {
        Off {
            parts: p.iter().map(|s| s.to_string()).collect(),
            constant,
        }
    }

    #[test]
    fn a_float_coordinate_moves_by_texels_over_the_level_size() {
        let s = shifted("c", &off(&["a", "b"], false), "t", false, &Level::Base);
        assert_eq!(
            s,
            "(c + float2(int2(a, b)) / float2(t.get_width(0u), t.get_height(0u)))"
        );
    }

    #[test]
    fn a_single_dimension_has_no_vector() {
        let s = shifted("c", &off(&["a"], false), "t", false, &Level::Float("l"));
        assert_eq!(s, "(c + float(a) / float(t.get_width(uint(max(l, 0.0f)))))");
    }

    #[test]
    fn a_fetch_adds_to_its_integer_coordinate() {
        let s = shifted(
            "uint2(x, y)",
            &off(&["1", "-2"], true),
            "t",
            true,
            &Level::Base,
        );
        assert_eq!(s, "uint2(int2(uint2(x, y)) + int2(1, -2))");
    }

    #[test]
    fn the_implicit_level_comes_from_the_sampler() {
        let s = shifted(
            "c",
            &off(&["a", "b", "d"], false),
            "t",
            false,
            &Level::Implicit("s"),
        );
        assert!(
            s.contains("t.get_depth(uint(t.calculate_clamped_lod(s, c)))"),
            "{s}"
        );
    }
}
