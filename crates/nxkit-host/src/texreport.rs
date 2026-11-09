//! The texture lines of the memory report: `nxkit-mem tex`. They say which Metal textures hold the bytes. The numbers
//! are the inventory that `nxkit-gpu` makes in the idle scan every 60 frames (`nxkit_gpu::inventory`), so they are
//! up to a second old. All sizes are MiB with one decimal, and the line building is in pure functions.
//!
//! The lines, in this order:
//! - `nxkit-mem tex store`: the Metal bytes the store holds against the bytes of all textures of the process
//!   (`nxkit_metal::mem_stats`, the difference is outside the store: staging and native textures), then count and
//!   MiB of each class (a texture is in the first class that fits: memoryless, attachment, gpu-written, cpu-read,
//!   scaled, widened, sampled), the block shadows, the retired textures (replaced or evicted, kept until the GPU is
//!   done) and the number of views (they share storage and add no bytes).
//! - `nxkit-mem tex guest`: for `sampled` and `widened`, the sum of the guest sizes against the Metal bytes and their
//!   quotient (the expansion).
//! - `nxkit-mem tex formats`: the 8 formats with the most bytes (count and MiB), the rest as `other`.
//! - `nxkit-mem tex idle`: the `sampled` class by frames since the last use. `<=60` is in use, `<=600` is not yet
//!   dropped by the default idle rule, `older` should have been dropped but was not.
//! - `nxkit-mem tex scale`: the scaled textures and what they would take at scale 1 (bytes divided by scale squared).
//! - `nxkit-mem tex dup`: entries that have the same guest start and description as another live entry, and entries
//!   whose guest range overlaps another live entry's range, each with the Metal textures behind them and their bytes.
//! - `nxkit-mem tex largest`: the largest Metal textures, one `format WxHxD/layers mips scale class MiB idle`.

use nxkit_gpu::inventory::{Class, Dup, Inventory, Total};

const MIB: f64 = 1024.0 * 1024.0;

/// Formats shown in the format line.
const TOP_FORMATS: usize = 8;

fn mib(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / MIB)
}

fn total(t: &Total) -> String {
    format!("{}/{}", t.count, mib(t.bytes))
}

fn store_line(i: &Inventory, process_bytes: u64) -> String {
    let held: u64 =
        i.classes.iter().map(|c| c.metal.bytes).sum::<u64>() + i.shadows.bytes + i.retired.bytes;
    let n: u64 = i.classes.iter().map(|c| c.metal.count).sum();
    let mut s = format!(
        "nxkit-mem tex store (count/MiB) frame {}: textures {} bytes {} of process {} |",
        i.frame,
        n,
        mib(held),
        mib(process_bytes)
    );
    for (c, t) in Class::ALL.iter().zip(&i.classes) {
        s += &format!(" {} {}", c.name(), total(&t.metal));
    }
    s + &format!(
        " | shadows {} retired {} views {}",
        total(&i.shadows),
        total(&i.retired),
        i.views
    )
}

fn guest_line(i: &Inventory) -> String {
    let part = |c: Class| {
        let t = &i.classes[c as usize];
        let x = if t.guest_bytes == 0 {
            "-".to_string()
        } else {
            format!("{:.2}", t.metal.bytes as f64 / t.guest_bytes as f64)
        };
        format!(
            "{} guest {} metal {} x{x}",
            c.name(),
            mib(t.guest_bytes),
            mib(t.metal.bytes)
        )
    };
    format!(
        "nxkit-mem tex guest (MiB): {} | {}",
        part(Class::Sampled),
        part(Class::Widened)
    )
}

fn formats_line(i: &Inventory) -> String {
    let mut s = String::from("nxkit-mem tex formats (count/MiB):");
    let mut rest = Total::default();
    for (k, (name, t)) in i.formats.iter().enumerate() {
        if k < TOP_FORMATS {
            s += &format!(" {name} {}", total(t));
        } else {
            rest.count += t.count;
            rest.bytes += t.bytes;
        }
    }
    if rest.count > 0 {
        s += &format!(" other {}", total(&rest));
    }
    s
}

fn idle_line(i: &Inventory) -> String {
    format!(
        "nxkit-mem tex idle (sampled, count/MiB): <=60 {} <=600 {} older {}",
        total(&i.idle[0]),
        total(&i.idle[1]),
        total(&i.idle[2])
    )
}

fn scale_line(i: &Inventory) -> String {
    format!(
        "nxkit-mem tex scale (count/MiB): scaled {} at-scale-1 {}",
        total(&i.scaled),
        mib(i.scaled_at_one)
    )
}

fn dup(d: &Dup) -> String {
    format!(
        "{} entries {} textures {} MiB",
        d.entries,
        d.textures,
        mib(d.bytes)
    )
}

fn dup_line(i: &Inventory) -> String {
    format!(
        "nxkit-mem tex dup: same-range {} | overlap {}",
        dup(&i.same_range),
        dup(&i.overlap)
    )
}

fn largest_line(i: &Inventory) -> String {
    let mut s = String::from("nxkit-mem tex largest (MiB):");
    for (k, l) in i.largest.iter().enumerate() {
        s += &format!(
            "{} {} {}x{}x{} m{} s{} {} {} idle {}",
            if k == 0 { "" } else { " ;" },
            l.format,
            l.width,
            l.height,
            l.depth_or_layers,
            l.mips,
            l.scale,
            l.class.name(),
            mib(l.bytes),
            l.idle
        );
    }
    s
}

/// The texture lines for an inventory. `process_bytes` is `texture_bytes` of the process.
pub fn tex_lines(i: &Inventory, process_bytes: u64) -> Vec<String> {
    vec![
        store_line(i, process_bytes),
        guest_line(i),
        formats_line(i),
        idle_line(i),
        scale_line(i),
        dup_line(i),
        largest_line(i),
    ]
}

/// The lines of the last scan, or none when there was no scan (the report is off, or fewer than 60 frames ran).
pub fn lines() -> Vec<String> {
    match nxkit_gpu::inventory::latest() {
        Some(i) => tex_lines(&i, nxkit_metal::mem_stats().texture_bytes),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_gpu::inventory::{ClassTotal, Largest};

    const M: u64 = 1024 * 1024;

    fn t(count: u64, mibs: u64) -> Total {
        Total {
            count,
            bytes: mibs * M,
        }
    }

    fn sample() -> Inventory {
        let mut i = Inventory {
            frame: 3600,
            views: 12,
            shadows: t(2, 4),
            retired: t(1, 8),
            ..Inventory::default()
        };
        i.classes[Class::Attachment as usize] = ClassTotal {
            metal: t(10, 100),
            guest_bytes: 0,
        };
        i.classes[Class::Sampled as usize] = ClassTotal {
            metal: t(40, 300),
            guest_bytes: 150 * M,
        };
        i.classes[Class::Widened as usize] = ClassTotal {
            metal: t(1, 8),
            guest_bytes: 0,
        };
        i.idle = [t(5, 30), t(15, 90), t(20, 180)];
        i.scaled = t(3, 90);
        i.scaled_at_one = 40 * M;
        i.same_range = Dup {
            entries: 6,
            textures: 3,
            bytes: 12 * M,
        };
        i.overlap = Dup {
            entries: 9,
            textures: 7,
            bytes: 50 * M,
        };
        i.largest = vec![
            Largest {
                format: "Rgba8Unorm".into(),
                width: 3840,
                height: 2160,
                depth_or_layers: 1,
                mips: 1,
                scale: 2.0,
                class: Class::Attachment,
                bytes: 32 * M,
                idle: 0,
            },
            Largest {
                format: "Bc(Bc7)".into(),
                width: 1024,
                height: 1024,
                depth_or_layers: 6,
                mips: 11,
                scale: 1.0,
                class: Class::Sampled,
                bytes: 8 * M,
                idle: 700,
            },
        ];
        i
    }

    #[test]
    fn the_store_line_has_every_class_and_the_sum() {
        let l = store_line(&sample(), 600 * M);
        for part in [
            "frame 3600",
            "textures 51 bytes 420.0 of process 600.0",
            "memoryless 0/0.0",
            "attachment 10/100.0",
            "gpu-written 0/0.0",
            "cpu-read 0/0.0",
            "scaled 0/0.0",
            "widened 1/8.0",
            "sampled 40/300.0",
            "shadows 2/4.0",
            "retired 1/8.0",
            "views 12",
        ] {
            assert!(l.contains(part), "{part} missing in {l}");
        }
        // the classes come in the order of precedence
        let pos = |p: &str| l.find(p).unwrap();
        assert!(pos("memoryless") < pos("attachment") && pos("widened") < pos("sampled"));
    }

    #[test]
    fn the_guest_line_shows_the_expansion() {
        let l = guest_line(&sample());
        assert!(l.contains("sampled guest 150.0 metal 300.0 x2.00"), "{l}");
        // no guest bytes known: no quotient
        assert!(l.contains("widened guest 0.0 metal 8.0 x-"), "{l}");
    }

    #[test]
    fn the_format_list_has_the_top_eight_and_the_rest() {
        let mut i = sample();
        i.formats = (0..11).map(|k| (format!("F{k}"), t(1, 100 - k))).collect();
        let l = formats_line(&i);
        assert!(l.contains("F0 1/100.0") && l.contains("F7 1/93.0"), "{l}");
        assert!(!l.contains("F8"), "{l}");
        // F8, F9 and F10 are 92 + 91 + 90
        assert!(l.ends_with("other 3/273.0"), "{l}");
        i.formats.truncate(2);
        assert!(!formats_line(&i).contains("other"));
    }

    #[test]
    fn the_idle_buckets_are_named() {
        let l = idle_line(&sample());
        assert!(
            l.ends_with("<=60 5/30.0 <=600 15/90.0 older 20/180.0"),
            "{l}"
        );
    }

    #[test]
    fn the_scale_and_duplicate_lines() {
        let i = sample();
        assert!(scale_line(&i).ends_with("scaled 3/90.0 at-scale-1 40.0"));
        let l = dup_line(&i);
        assert!(
            l.contains("same-range 6 entries 3 textures 12.0 MiB"),
            "{l}"
        );
        assert!(l.contains("overlap 9 entries 7 textures 50.0 MiB"), "{l}");
    }

    #[test]
    fn the_largest_line_lists_each_texture() {
        let l = largest_line(&sample());
        assert!(
            l.contains("Rgba8Unorm 3840x2160x1 m1 s2 attachment 32.0 idle 0 ;"),
            "{l}"
        );
        assert!(
            l.ends_with("Bc(Bc7) 1024x1024x6 m11 s1 sampled 8.0 idle 700"),
            "{l}"
        );
    }

    #[test]
    fn every_line_starts_with_the_prefix() {
        let lines = tex_lines(&sample(), 0);
        assert_eq!(lines.len(), 7);
        assert!(lines.iter().all(|l| l.starts_with("nxkit-mem tex ")));
        assert!(tex_lines(&Inventory::default(), 0).len() == 7);
    }
}
