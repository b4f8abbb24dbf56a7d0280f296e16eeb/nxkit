use super::*;

/// A vr network of the 10-pixel scale (variant A) and a vr_light one (3-layer weights).
const VR_A: &str = "14c10aa70b81f04e";
const VR_B: &str = "f407ab4429b8eae6";
const LIGHT: &str = "927ceaf3cf1be240";
/// Ocarina of Time's network: vr_light 4/3, variant B/C.
const OCARINA: &str = "fa3033d05c0a41d6";
/// An agl_ndsr network (no kernels).
const AGL: &str = "abef20ad0d9a9b12";

fn bound(resource: GameResource, w: u32, h: u32, format: Format) -> Bound {
    Bound {
        resource,
        width: w,
        height: h,
        format,
    }
}

/// A push block of 20 words with the vr layout of section 7.9.
fn push(w_in: u32, h_in: u32, w_out: u32, h_out: u32) -> Vec<u8> {
    let mut words = [0u32; 20];
    words[2] = w_in - 1;
    words[3] = h_in - 1;
    words[6] = w_out - 1;
    words[7] = h_out - 1;
    words[8] = (1.0f32 / w_in as f32).to_bits();
    words[9] = (1.0f32 / h_in as f32).to_bits();
    words[18] = 1.5f32.to_bits();
    words[19] = 1;
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn info<'a>(key: &'a str, push: &'a [u8], out: Format, size_out: [u32; 2]) -> RunInfo<'a> {
    RunInfo {
        keys: vec![key],
        pushes: vec![Some(push)],
        bound: vec![
            bound(INPUT, 1536, 864, Format::Rgba16Float),
            bound(OUTPUT, size_out[0], size_out[1], out),
        ],
    }
}

#[test]
fn the_run_is_the_network_dispatch_among_the_smaa_ones() {
    let n = Ndsr;
    let keys = ["f8965bff", "d17eefaf", "92918e9b", VR_A, "x"];
    assert_eq!(n.find(&keys), Some(Run { start: 3, len: 1 }));
    assert_eq!(n.find(&["f8965bff", "92918e9b"]), None);
    assert_eq!(n.name(), "ndsr");
    assert_eq!(n.lead(), 3);
}

#[test]
fn the_resources_stay_inside_the_one_dispatch_run() {
    for r in Ndsr.resources() {
        let d = match r.place {
            Place::Texture { dispatch, .. }
            | Place::Image { dispatch, .. }
            | Place::Push { dispatch } => dispatch,
        };
        assert_eq!(d, 0, "{r:?}");
    }
    for b in Ndsr.blobs() {
        assert_eq!(b.dispatch, 0);
    }
}

#[test]
fn the_blob_length_follows_the_network() {
    let p = push(1536, 864, 1920, 1080);
    let s = shape_of(&info(VR_A, &p, Format::Rgba8Unorm, [1920, 1080]));
    assert_eq!(s.blobs[0].len, 0x3000);
    assert_eq!(s.refusal, None);
    // A vr_light program: its 3-layer blob.
    let s = shape_of(&info(LIGHT, &p, Format::Rgba8Unorm, [1920, 1080]));
    assert_eq!(s.blobs[0].len, 0x1800);
    assert_eq!(s.refusal, None);
    // agl_ndsr has no kernels: refused with a reason, its blob is still told.
    let s = shape_of(&info(AGL, &p, Format::Rgba8Unorm, [1920, 1080]));
    assert_eq!(s.blobs[0].len, 0x3000);
    assert!(s.refusal.is_some_and(|r| r.contains("not run natively")));
}

/// The 29 push words of Ocarina of Time's network dispatch (`fa3033d05c0a41d6`, 1536x864 -> 2048x1152), as the
/// capture of ndsr-native.md section 10 logged them.
fn ocarina_push() -> Vec<u8> {
    let words: [u32; 29] = [
        0,
        0,
        0x5ff,
        0x35f,
        0,
        0,
        0x7ff,
        0x47f,
        0x3a2a_aaab,
        0x3a97_b426,
        0x3a00_0000,
        0x3a63_8e39,
        0x3a00_1559,
        0x3a63_d1b9,
        0x3f7f_d555,
        0x3f7f_b426,
        0x39aa_aaab,
        0x3a17_b426,
        0x40,
        0x20,
        0x3f80_0000,
        0x3f80_0000,
        0,
        0,
        0x800,
        0x480,
        0x3a00_0000,
        0x3a63_8e39,
        0x4020_0000,
    ];
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

#[test]
fn ocarina_of_time_runs_natively() -> Result<(), String> {
    let p = ocarina_push();
    let n = Ndsr;
    let keys = [
        "f8965bffbdf51d78",
        "d17eefaff4f0bc23",
        "a335c1db6270c355",
        OCARINA,
    ];
    assert_eq!(n.find(&keys), Some(Run { start: 3, len: 1 }));
    let run = |p: &'_ [u8], out: Format| -> Shape {
        shape_of(&RunInfo {
            keys: vec![OCARINA],
            pushes: vec![Some(p)],
            bound: vec![
                bound(INPUT, 1536, 864, Format::Rgb9E5Float),
                bound(OUTPUT, 2048, 1152, out),
            ],
        })
    };
    let s = run(&p, Format::Rg11B10Float);
    assert_eq!(s.refusal, None);
    assert_eq!(&s.tag[2..6], &[1536, 864, 2048, 1152]);
    let info = RunInfo {
        keys: vec![OCARINA],
        pushes: vec![Some(&p)],
        bound: vec![
            bound(INPUT, 1536, 864, Format::Rgb9E5Float),
            bound(OUTPUT, 2048, 1152, Format::Rg11B10Float),
        ],
    };
    let plan = n.plan_in(&[&[0; 0x1800]], &s, &info)?;
    assert_eq!(plan.steps[0].kernel, "ndsr_light_r43_b");
    assert_eq!(plan.steps[0].groups, [32, 36, 1]);
    // The sharpening strength makes a new plan, a word the light programs do not read does not.
    let mut q = p.clone();
    q[12 * 4] ^= 1;
    assert_eq!(run(&q, Format::Rg11B10Float).tag, s.tag);
    q[28 * 4] ^= 1;
    assert_ne!(run(&q, Format::Rg11B10Float).tag, s.tag);
    // An 8-bit image is not what the linear program writes.
    assert!(
        run(&p, Format::Rgba8Unorm)
            .refusal
            .is_some_and(|w| w.contains("img1"))
    );
    Ok(())
}

#[test]
fn each_size_and_network_gets_its_own_tag() {
    let p1 = push(1536, 864, 1920, 1080);
    let p2 = push(1280, 720, 1920, 1080);
    let tag = |key, p: &[u8], out| shape_of(&info(key, p, out, [1920, 1080])).tag;
    let a = tag(VR_A, &p1, Format::Rgba8Unorm);
    assert_eq!(a, tag(VR_A, &p1, Format::Rgba8Unorm), "stable");
    assert_ne!(a, tag(VR_A, &p2, Format::Rgba8Unorm), "input size");
    assert_ne!(a, tag(VR_B, &p1, Format::Rgb10A2Unorm), "network");
    assert_eq!(&a[2..6], &[1536, 864, 1920, 1080]);
    // The words the kernel does not read do not make a new plan; the strength does.
    let mut q = p1.clone();
    q[0] ^= 0xff;
    q[12 * 4] ^= 0xff;
    assert_eq!(a, tag(VR_A, &q, Format::Rgba8Unorm));
    q[18 * 4] ^= 0x01;
    assert_ne!(a, tag(VR_A, &q, Format::Rgba8Unorm));
}

#[test]
fn the_formats_must_be_the_variants() {
    let p = push(1536, 864, 1920, 1080);
    // Variant A writes 8 bit; a 10 bit image is variant B's.
    let s = shape_of(&info(VR_A, &p, Format::Rgb10A2Unorm, [1920, 1080]));
    assert!(s.refusal.is_some_and(|r| r.contains("img1")));
    // The capture still gets the tag.
    assert_eq!(s.tag.len(), 8);
    let mut i = info(VR_A, &p, Format::Rgba8Unorm, [1920, 1080]);
    i.bound[0].format = Format::Rgba8Unorm;
    assert!(shape_of(&i).refusal.is_some_and(|r| r.contains("tex0")));
}

#[test]
fn a_run_without_pictures_or_push_is_refused() {
    let p = push(1536, 864, 1920, 1080);
    let mut i = info(VR_A, &p, Format::Rgba8Unorm, [1920, 1080]);
    i.bound.pop();
    assert!(shape_of(&i).refusal.is_some_and(|r| r.contains("img1")));
    let mut i = info(VR_A, &p, Format::Rgba8Unorm, [1920, 1080]);
    i.pushes = vec![None];
    assert!(shape_of(&i).refusal.is_some_and(|r| r.contains("push")));
    let i = info("zzzz", &p, Format::Rgba8Unorm, [1920, 1080]);
    assert!(shape_of(&i).refusal.is_some_and(|r| r.contains("zzzz")));
}

#[test]
fn the_sizes_come_from_the_push_words_when_they_fit() {
    let i = bound(INPUT, 1536, 864, Format::Rgba16Float);
    let o = bound(OUTPUT, 1920, 1080, Format::Rgba8Unorm);
    let p = push(1280, 720, 1920, 1080);
    assert_eq!(plan_sizes(&p, &i, &o), ([1280, 720], [1920, 1080]));
    // Larger than the picture, or no words: the pictures' sizes.
    let big = push(2000, 720, 1920, 1080);
    assert_eq!(plan_sizes(&big, &i, &o).0, [1536, 864]);
    assert_eq!(plan_sizes(&[], &i, &o), ([1536, 864], [1920, 1080]));
}

#[test]
fn the_weights_and_the_plan_follow_the_network_and_the_sizes() -> Result<(), String> {
    let n = Ndsr;
    let p = push(1280, 720, 1920, 1080);
    let i = info(VR_A, &p, Format::Rgba8Unorm, [1920, 1080]);
    let s = n.shape(&i);
    // Zero bytes are not the game's weights.
    assert!(!n.accepts_in(&[&[0; 0x3000]], &s, &i));
    assert!(!n.accepts_in(&[&[0; 0x1800]], &s, &i));
    assert!(!n.accepts_in(&[], &s, &i));
    let plan = n.plan_in(&[&[0; 0x3000]], &s, &i)?;
    assert_eq!(plan.steps[0].kernel, "ndsr_in10_a");
    assert_eq!(
        plan.steps[0].groups,
        [1280u32.div_ceil(39), 720u32.div_ceil(29), 1]
    );
    // A vr_light plan needs its 29 push words.
    let i = info(LIGHT, &p, Format::Rgba8Unorm, [1920, 1080]);
    assert!(n.plan_in(&[&[0; 0x1800]], &s, &i).is_err());
    assert!(n.plan_in(&[], &s, &i).is_err());
    // The static interface is not usable: the plan needs the sizes.
    assert!(!n.accepts(&[&[0; 0x3000]]));
    assert!(n.plan(&[&[0; 0x3000]]).is_err());
    Ok(())
}
