//! Tests for the texture inventory.

use super::*;
use nxkit_metal::Format;

fn desc(format: Format, w: u32, h: u32) -> TextureDesc {
    TextureDesc {
        kind: TextureKind::D2,
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        array_layers: 1,
        allow_views: true,
        format,
        shader_read: true,
        shader_write: false,
        render_target: false,
        storage: Storage::Private,
        shader_atomic: false,
    }
}

fn tex(class: Class, format: Format, bytes: u64, idle: u64) -> Tex {
    Tex {
        class,
        desc: desc(format, 64, 64),
        bytes,
        guest: bytes / 2,
        scale: 1.0,
        idle,
    }
}

fn span(address: u64, size: u64, generation: u64, w: u32) -> Span {
    Span {
        address,
        size,
        desc: desc(Format::Rgba8Unorm, w, w),
        generation,
        bytes: 100,
        attachment: false,
        idle: 0,
    }
}

fn att(address: u64, size: u64, generation: u64, w: u32, idle: u64) -> Span {
    Span {
        attachment: true,
        idle,
        ..span(address, size, generation, w)
    }
}

#[test]
fn the_first_class_that_fits_wins() {
    let all = Flags {
        memoryless: true,
        attachment: true,
        atomic: true,
        cpu_read: true,
        gpu_written: true,
        scaled: true,
        widened: true,
    };
    assert_eq!(classify(&all), Class::Memoryless);
    let f = |g: fn(&mut Flags)| {
        let mut f = all;
        f.memoryless = false;
        g(&mut f);
        classify(&f)
    };
    assert_eq!(f(|_| {}), Class::Attachment);
    assert_eq!(f(|f| f.attachment = false), Class::GpuWritten);
    assert_eq!(
        f(|f| {
            f.attachment = false;
            f.gpu_written = false;
        }),
        Class::GpuWritten,
        "atomics count as written"
    );
    assert_eq!(
        f(|f| {
            f.attachment = false;
            f.gpu_written = false;
            f.atomic = false;
        }),
        Class::CpuRead
    );
    assert_eq!(
        f(|f| *f = Flags {
            scaled: true,
            widened: true,
            ..Flags::default()
        }),
        Class::Scaled
    );
    assert_eq!(
        f(|f| *f = Flags {
            widened: true,
            ..Flags::default()
        }),
        Class::Widened
    );
    assert_eq!(f(|f| *f = Flags::default()), Class::Sampled);
}

#[test]
fn classes_formats_and_idle_buckets_add_up() {
    let texs = [
        tex(Class::Sampled, Format::Rgba8Unorm, 1000, 0),
        tex(Class::Sampled, Format::Rgba8Unorm, 2000, 60),
        tex(Class::Sampled, Format::Rgba8Unorm, 4000, 61),
        tex(Class::Sampled, Format::Rgba8Unorm, 8000, 600),
        tex(Class::Sampled, Format::Rgba8Unorm, 16000, 601),
        tex(Class::Attachment, Format::Rgba16Float, 500, 9000),
    ];
    let inv = build(120, &texs, &[], 3, Total::default(), Total::default());
    let sampled = inv.classes[Class::Sampled as usize];
    assert_eq!(
        sampled.metal,
        Total {
            count: 5,
            bytes: 31000
        }
    );
    assert_eq!(sampled.guest_bytes, 15500);
    assert_eq!(inv.classes[Class::Attachment as usize].metal.count, 1);
    assert_eq!(
        inv.idle[0],
        Total {
            count: 2,
            bytes: 3000
        }
    );
    assert_eq!(
        inv.idle[1],
        Total {
            count: 2,
            bytes: 12000
        }
    );
    assert_eq!(
        inv.idle[2],
        Total {
            count: 1,
            bytes: 16000
        }
    );
    assert_eq!(inv.formats.len(), 2);
    assert_eq!(inv.formats[0].0, "Rgba8Unorm");
    assert_eq!(inv.formats[0].1.bytes, 31000);
    assert_eq!(inv.largest[0].bytes, 16000);
    assert_eq!(inv.largest.last().map(|l| l.bytes), Some(500));
    assert_eq!(inv.views, 3);
}

#[test]
fn only_the_largest_are_kept() {
    let texs: Vec<Tex> = (0..20)
        .map(|i| tex(Class::Sampled, Format::Rgba8Unorm, 100 + i, 0))
        .collect();
    let inv = build(0, &texs, &[], 0, Total::default(), Total::default());
    assert_eq!(inv.largest.len(), LARGEST);
    assert_eq!(inv.largest[0].bytes, 119);
}

#[test]
fn a_scaled_texture_shows_its_size_at_scale_one() {
    let mut t = tex(Class::Attachment, Format::Rgba8Unorm, 4000, 0);
    t.scale = 2.0;
    let inv = build(0, &[t], &[], 0, Total::default(), Total::default());
    assert_eq!(
        inv.scaled,
        Total {
            count: 1,
            bytes: 4000
        }
    );
    assert_eq!(inv.scaled_at_one, 1000);
}

#[test]
fn entries_with_one_range_and_description_are_duplicates() {
    // two ids share a Metal texture (generation 1), a third has its own at the same range, a fourth differs
    let spans = [
        span(0x1000, 0x400, 1, 16),
        span(0x1000, 0x400, 1, 16),
        span(0x1000, 0x400, 2, 16),
        span(0x1000, 0x400, 3, 32),
        span(0x9000, 0x400, 4, 16),
    ];
    let d = same_range(&spans);
    assert_eq!(
        d,
        Dup {
            entries: 3,
            textures: 2,
            bytes: 200
        }
    );
}

#[test]
fn overlapping_ranges_are_found_at_both_ends() {
    let spans = [
        span(0x1000, 0x1000, 1, 16), // overlaps the next two
        span(0x1800, 0x100, 2, 16),  // inside the first
        span(0x1c00, 0x1000, 3, 16), // overlaps the first
        span(0x3000, 0x100, 4, 16),  // alone
        span(0x3100, 0x100, 5, 16),  // touches the one before, no overlap
        span(0x8000, 0, 6, 16),      // empty
    ];
    let d = overlap(&spans);
    assert_eq!(
        d,
        Dup {
            entries: 3,
            textures: 3,
            bytes: 300
        }
    );
}

#[test]
fn the_overlap_is_split_into_attachment_entries_and_the_others() {
    let spans = [
        att(0x1000, 0x1000, 1, 16, 0), // overlaps the next two
        span(0x1800, 0x100, 2, 16),    // inside the first
        att(0x1c00, 0x1000, 1, 16, 0), // overlaps the first, shares its Metal texture
        att(0x9000, 0x100, 4, 16, 0),  // alone
    ];
    let (a, o) = overlap_split(&spans);
    assert_eq!(
        a,
        Dup {
            entries: 2,
            textures: 1,
            bytes: 100
        }
    );
    assert_eq!(
        o,
        Dup {
            entries: 1,
            textures: 1,
            bytes: 100
        }
    );
    // the two parts add up to the whole
    let all = overlap(&spans);
    assert_eq!((all.entries, all.bytes), (3, 200));
}

#[test]
fn attachment_entries_are_counted_by_idle_age() {
    let spans = [
        att(0x1000, 0x100, 1, 16, 0),
        att(0x2000, 0x100, 1, 16, 0), // same Metal texture: counted as an entry, its bytes once
        att(0x3000, 0x100, 2, 16, 60),
        att(0x4000, 0x100, 3, 16, 61),
        att(0x5000, 0x100, 4, 16, 600),
        att(0x6000, 0x100, 5, 16, 601),
        span(0x7000, 0x100, 6, 16), // not an attachment
    ];
    let (idle, _) = attachments(&spans);
    let t = |count, bytes| Total { count, bytes };
    assert_eq!(idle, [t(3, 200), t(2, 200), t(1, 100)]);
}

#[test]
fn the_shapes_of_the_attachments_are_distinct_by_format_width_and_height() {
    let mut other_format = att(0x3000, 0x100, 3, 16, 0);
    other_format.desc.format = Format::Bgra8Unorm;
    let spans = [
        att(0x1000, 0x100, 1, 16, 0),
        att(0x2000, 0x100, 2, 16, 0), // same shape as the first
        other_format,
        att(0x4000, 0x100, 4, 32, 0),
        span(0x5000, 0x100, 5, 64), // a sampled entry adds no shape
    ];
    assert_eq!(attachments(&spans).1, 3);
    assert_eq!(attachments(&[]).1, 0);
}

#[test]
fn build_fills_the_attachment_fields() {
    let spans = [att(0x1000, 0x200, 1, 16, 5), att(0x1100, 0x200, 2, 16, 700)];
    let inv = build(10, &[], &spans, 0, Total::default(), Total::default());
    assert_eq!(inv.attachment_idle[0].count, 1);
    assert_eq!(inv.attachment_idle[2].count, 1);
    assert_eq!(inv.attachment_shapes, 1);
    assert_eq!(inv.overlap_attachment.entries, 2);
    assert_eq!(inv.overlap_other.entries, 0);
}
