//! Tests for the support block of the argument buffers.

use super::*;
use crate::programs::Bind;

fn bind(kind: BindKind, reg: u32, rscale: Option<u32>) -> Bind {
    Bind {
        arg_id: 0,
        bank: 0,
        kind,
        reg,
        dim: None,
        rw: false,
        rscale,
        img: None,
    }
}

fn env<'a>(
    handles: &'a crate::draws::FastMap<u64, Resolved>,
    fits: &'a crate::draws::FastMap<(u64, nxkit_metal::TextureKind), u64>,
    images: &'a crate::draws::FastMap<ImageKey, ImageRes>,
) -> Env<'a> {
    static NO_POOLS: std::sync::OnceLock<crate::guestpools::GuestPools> =
        std::sync::OnceLock::new();
    Env {
        handles,
        fits,
        images,
        gmem: crate::gmem::GmemEnv::NONE,
        pools: (0, 0),
        guest_pools: NO_POOLS.get_or_init(crate::guestpools::GuestPools::empty),
        default_sampler: 0,
        statics: (0, 0),
        scale: 2.0,
    }
}

fn entry(b: &[u8], i: usize) -> f32 {
    let at = SUPPORT_RENDER_SCALE as usize + 16 * i;
    f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

#[test]
fn native_size_needs_no_block() {
    let (h, f, i) = Default::default();
    let binds = [
        bind(BindKind::Support, 0, Some(0)),
        bind(BindKind::Tex, 3, Some(1)),
    ];
    let mut b = Vec::new();
    assert!(!support_block(
        &mut b,
        &binds,
        &env(&h, &f, &i),
        1.0,
        &|_| None,
        &|_| None
    ));
}

#[test]
fn the_pass_and_the_scaled_texture_get_their_entries() {
    let mut h: crate::draws::FastMap<u64, Resolved> = Default::default();
    h.insert(
        7,
        Resolved {
            texture: 1,
            sampler: 1,
            scaled: true,
        },
    );
    h.insert(
        8,
        Resolved {
            texture: 2,
            sampler: 2,
            scaled: false,
        },
    );
    let (f, i) = Default::default();
    let binds = [
        bind(BindKind::Support, 0, Some(0)),
        bind(BindKind::Tex, 3, Some(1)),
        bind(BindKind::Tex, 4, Some(2)),
        bind(BindKind::Tex, 5, None),
    ];
    let handle = |slot: u32| match slot {
        3 => Some(7),
        4 => Some(8),
        _ => None,
    };
    let mut b = Vec::new();
    assert!(support_block(
        &mut b,
        &binds,
        &env(&h, &f, &i),
        2.0,
        &handle,
        &|_| None
    ));
    assert_eq!(b.len(), SUPPORT_SIZE);
    assert_eq!(
        (entry(&b, 0), entry(&b, 1), entry(&b, 2), entry(&b, 3)),
        (2.0, 2.0, 1.0, 1.0)
    );
    // frag_scale_count stays 0
    assert_eq!(&b[176..180], &[0; 4]);
}

#[test]
fn a_scaled_image_gets_its_entry() {
    use crate::programs::{ImgAccess, ImgDecl};
    use nxkit_metal::TextureKind;
    let decl = ImgDecl {
        access: ImgAccess::Write,
        atomic: false,
        signed: false,
    };
    let mut img = bind(BindKind::Img, 0, Some(3));
    img.dim = Some(TextureKind::D2);
    img.img = Some(decl);
    let (h, f) = Default::default();
    let mut images: crate::draws::FastMap<ImageKey, ImageRes> = Default::default();
    images.insert(
        (5, TextureKind::D2, 0),
        ImageRes {
            id: 9,
            scaled: true,
        },
    );
    let mut b = Vec::new();
    let at = |slot: u32| (slot == 0).then_some(5);
    assert!(support_block(
        &mut b,
        &[img],
        &env(&h, &f, &images),
        1.0,
        &|_| None,
        &at
    ));
    assert_eq!(entry(&b, 3), 2.0);
    // an image nothing is bound to keeps scale 1
    assert!(!support_block(
        &mut b,
        &[img],
        &env(&h, &f, &images),
        1.0,
        &|_| None,
        &|_| None
    ));
}
