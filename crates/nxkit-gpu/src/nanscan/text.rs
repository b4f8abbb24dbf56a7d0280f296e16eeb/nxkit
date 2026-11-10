//! The counters of a record and the text of the `nanscan` lines.

use nxkit_metal::Format;
use nxkit_nvn::resource::TexId;

use super::kernel::{FIRST, FIRST_AT, NAN_AT, SLICE_AT, SLICES};

/// The slices a line names.
const SLICES_SHOWN: usize = 16;

/// What the kernel counted in one texture.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Counts {
    /// The texels with a non-finite channel.
    pub(super) total: u32,
    /// The texels with a non-finite R, G, B and A.
    pub(super) channel: [u32; 4],
    /// The texels with a NaN in R, G, B and A.
    pub(super) nan: [u32; 4],
    /// The +Inf and -Inf values of R, G and B together.
    pub(super) inf: [u32; 2],
    /// The texels with a non-finite RGB, by alpha: exactly 0, finite and not 0, non-finite.
    pub(super) alpha: [u32; 3],
    /// The first texels the kernel found: x, y and slice.
    pub(super) first: Vec<[u32; 3]>,
    /// The slices that hold non-finite texels, with their counts, in slice order.
    pub(super) slices: Vec<(u32, u32)>,
}

impl Counts {
    /// Reads a record of `WORDS` words.
    pub(super) fn from_words(w: &[u32]) -> Self {
        let total = w[0];
        Self {
            total,
            channel: [w[1], w[2], w[3], w[4]],
            nan: [w[NAN_AT], w[NAN_AT + 1], w[NAN_AT + 2], w[NAN_AT + 3]],
            inf: [w[NAN_AT + 4], w[NAN_AT + 5]],
            alpha: [w[5], w[6], w[7]],
            first: (0..(total as usize).min(FIRST))
                .map(|i| {
                    let at = FIRST_AT + 3 * i;
                    [w[at], w[at + 1], w[at + 2]]
                })
                .collect(),
            slices: (0..SLICES)
                .filter(|z| w[SLICE_AT + z] > 0)
                .map(|z| (z as u32, w[SLICE_AT + z]))
                .collect(),
        }
    }
}

/// The texture a line is about.
pub(super) struct Subject {
    pub(super) id: TexId,
    pub(super) format: Format,
    pub(super) size: [u32; 2],
    /// The depth of a 3D texture or the layers of an array, a cube or a cube array. `None` for a 2D texture.
    pub(super) depth: Option<u32>,
}

impl Subject {
    pub(super) fn size_text(&self) -> String {
        match self.depth {
            Some(d) => format!("{}x{}x{d}", self.size[0], self.size[1]),
            None => format!("{}x{}", self.size[0], self.size[1]),
        }
    }
}

/// The channels a float format has.
pub(super) fn channels_of(f: Format) -> usize {
    match f {
        Format::Rgba16Float | Format::Rgba32Float => 4,
        Format::Rg11B10Float => 3,
        Format::Rg16Float | Format::Rg32Float => 2,
        _ => 1,
    }
}

/// The part of a line after the total: the channels the format has, the NaN and Inf counts, the alpha classes when it has alpha, the count
/// before the pass, the first texels and the slices.
fn counts_text(c: &Counts, channels: usize, layered: bool, load: Option<u32>) -> String {
    let mut s = format!(" nonfinite {}", c.total);
    for (name, n) in ["r", "g", "b", "a"].iter().zip(c.channel).take(channels) {
        s.push_str(&format!(" {name} {n}"));
    }
    s.push_str(" | nan");
    for (name, n) in ["r", "g", "b", "a"].iter().zip(c.nan).take(channels) {
        s.push_str(&format!(" {name} {n}"));
    }
    s.push_str(&format!(" inf+ {} inf- {}", c.inf[0], c.inf[1]));
    if channels == 4 {
        s.push_str(&format!(
            " | rgb with alpha 0: {}, finite: {}, non-finite: {}",
            c.alpha[0], c.alpha[1], c.alpha[2]
        ));
    }
    if let Some(l) = load {
        s.push_str(&format!(" at load {l}"));
    }
    s.push_str(" first");
    for f in &c.first {
        if layered {
            s.push_str(&format!(" ({},{},{})", f[0], f[1], f[2]));
        } else {
            s.push_str(&format!(" ({},{})", f[0], f[1]));
        }
    }
    if layered {
        let shown: Vec<String> = c
            .slices
            .iter()
            .take(SLICES_SHOWN)
            .map(|(z, n)| format!("{z}: {n}"))
            .collect();
        s.push_str(&format!(" slices [{}]", shown.join(", ")));
        let left = c.slices.len().saturating_sub(SLICES_SHOWN);
        if left > 0 {
            s.push_str(&format!(" left out {left}"));
        }
    }
    s
}

/// The line of a colour target after a pass. `load` is the count before the pass, when it was counted.
pub(super) fn target_line(
    frame: u64,
    pass: usize,
    prog: &str,
    s: &Subject,
    c: &Counts,
    load: Option<u32>,
) -> String {
    format!(
        "DIAG nanscan frame {frame} pass {pass} prog {prog} target {:?} {:?} {}{}",
        s.id,
        s.format,
        s.size_text(),
        counts_text(c, channels_of(s.format), s.depth.is_some(), load)
    )
}

/// The line of a texture that a program of the pass samples.
pub(super) fn sample_line(
    frame: u64,
    pass: usize,
    prog: &str,
    s: &Subject,
    stage: &str,
    slot: u32,
    c: &Counts,
) -> String {
    format!(
        "DIAG nanscan frame {frame} pass {pass} prog {prog} samples {:?} {:?} {} stage {stage} slot {slot}{}",
        s.id,
        s.format,
        s.size_text(),
        counts_text(c, channels_of(s.format), s.depth.is_some(), None)
    )
}

/// The line of a storage image of a compute pass after the pass. `load` is the count before the pass.
pub(super) fn image_line(
    frame: u64,
    pass: usize,
    prog: &str,
    s: &Subject,
    slot: u32,
    c: &Counts,
    load: Option<u32>,
) -> String {
    format!(
        "DIAG nanscan frame {frame} pass {pass} prog {prog} image {:?} {:?} {} slot {slot}{}",
        s.id,
        s.format,
        s.size_text(),
        counts_text(c, channels_of(s.format), s.depth.is_some(), load)
    )
}

/// The two lines that list every fragment program and every mesh program of a pass.
pub(super) fn programs_lines(fragment: &[String], mesh: &[String]) -> [String; 2] {
    let list = |keys: &[String]| {
        if keys.is_empty() {
            "-".to_owned()
        } else {
            keys.join(",")
        }
    };
    [
        format!(
            "DIAG nanscan pass programs ({}): {}",
            fragment.len(),
            list(fragment)
        ),
        format!(
            "DIAG nanscan pass mesh programs ({}): {}",
            mesh.len(),
            list(mesh)
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::kernel::WORDS;
    use super::*;

    fn record() -> Vec<u32> {
        let mut w = vec![0u32; WORDS];
        w[0] = 10;
        w[1..5].copy_from_slice(&[10, 9, 8, 1]);
        w[5..8].copy_from_slice(&[6, 3, 1]);
        w[NAN_AT..NAN_AT + 6].copy_from_slice(&[4, 3, 2, 1, 5, 6]);
        w[FIRST_AT..FIRST_AT + 6].copy_from_slice(&[1, 2, 0, 3, 4, 5]);
        w
    }

    fn subject(format: Format, depth: Option<u32>) -> Subject {
        Subject {
            id: TexId(7),
            format,
            size: [768, 432],
            depth,
        }
    }

    #[test]
    fn a_record_reads_back() {
        let mut w = record();
        w[SLICE_AT + 3] = 4;
        w[SLICE_AT + 5] = 6;
        let c = Counts::from_words(&w);
        assert_eq!(c.total, 10);
        assert_eq!(c.channel, [10, 9, 8, 1]);
        assert_eq!(c.alpha, [6, 3, 1]);
        assert_eq!(c.nan, [4, 3, 2, 1]);
        assert_eq!(c.inf, [5, 6]);
        assert_eq!(
            c.first,
            vec![[1, 2, 0], [3, 4, 5]]
                .into_iter()
                .chain((2..8).map(|_| [0, 0, 0]))
                .collect::<Vec<_>>()
        );
        assert_eq!(c.slices, vec![(3, 4), (5, 6)]);
    }

    #[test]
    fn a_four_channel_line_has_channels_and_alpha_classes() {
        let mut w = record();
        w[0] = 2;
        let c = Counts::from_words(&w);
        let l = target_line(
            9,
            4,
            "abc",
            &subject(Format::Rgba16Float, None),
            &c,
            Some(2),
        );
        assert_eq!(
            l,
            "DIAG nanscan frame 9 pass 4 prog abc target TexId(7) Rgba16Float 768x432 nonfinite 2 r 10 g 9 b 8 a 1 | nan r 4 g 3 b 2 a 1 inf+ 5 inf- 6 | rgb with alpha 0: 6, finite: 3, non-finite: 1 at load 2 first (1,2) (3,4)"
        );
    }

    #[test]
    fn a_format_without_alpha_prints_its_channels_only() {
        let c = Counts::from_words(&record());
        let rgb = target_line(1, 0, "p", &subject(Format::Rg11B10Float, None), &c, None);
        assert!(rgb.contains("nonfinite 10 r 10 g 9 b 8 | nan r 4 g 3 b 2 inf+ 5 inf- 6 first"), "{rgb}");
        assert!(!rgb.contains(" a ") && !rgb.contains("alpha"), "{rgb}");
        assert!(!rgb.contains("at load"), "{rgb}");
        let rg = target_line(1, 0, "p", &subject(Format::Rg16Float, None), &c, None);
        assert!(rg.contains("nonfinite 10 r 10 g 9 | nan r 4 g 3 inf+ 5 inf- 6 first"), "{rg}");
        let r = target_line(1, 0, "p", &subject(Format::R32Float, None), &c, None);
        assert!(r.contains("nonfinite 10 r 10 | nan r 4 inf+ 5 inf- 6 first"), "{r}");
    }

    #[test]
    fn a_layered_line_names_its_slices() {
        let mut w = record();
        for z in 0..20 {
            w[SLICE_AT + z] = z as u32 + 1;
        }
        let c = Counts::from_words(&w);
        let l = target_line(1, 2, "p", &subject(Format::Rgba16Float, Some(32)), &c, None);
        assert!(l.contains(" 768x432x32 "), "{l}");
        assert!(l.contains("first (1,2,0) (3,4,5)"), "{l}");
        assert!(l.contains("slices [0: 1, 1: 2, 2: 3,"), "{l}");
        assert!(l.contains("15: 16] left out 4"), "{l}");
        let few = Counts::from_words(&record());
        let l = target_line(
            1,
            2,
            "p",
            &subject(Format::Rgba16Float, Some(2)),
            &few,
            None,
        );
        assert!(l.ends_with("slices []"), "{l}");
    }

    #[test]
    fn a_sample_line_names_the_stage_and_the_slot() {
        let c = Counts::from_words(&record());
        let l = sample_line(
            3,
            29,
            "key",
            &subject(Format::Rgba16Float, Some(8)),
            "fs",
            2,
            &c,
        );
        assert!(
            l.starts_with(
                "DIAG nanscan frame 3 pass 29 prog key samples TexId(7) Rgba16Float 768x432x8 stage fs slot 2 nonfinite 10 r 10"
            ),
            "{l}"
        );
    }

    #[test]
    fn the_programs_lines_list_every_key() {
        let f = vec!["a".to_owned(), "b".to_owned()];
        let [one, two] = programs_lines(&f, &[]);
        assert_eq!(one, "DIAG nanscan pass programs (2): a,b");
        assert_eq!(two, "DIAG nanscan pass mesh programs (0): -");
        let [_, two] = programs_lines(&f, &f);
        assert_eq!(two, "DIAG nanscan pass mesh programs (2): a,b");
    }
}
