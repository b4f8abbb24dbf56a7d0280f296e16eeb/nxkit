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

static NO_RENAME: crate::rename::RenameMap = crate::rename::RenameMap::new();

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
        rename: &NO_RENAME,
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

/// A backing whose real address lies a fixed distance from the guest address, so a word and a c0 value differ.
struct Shifted(u64);

impl Backing for Shifted {
    fn real_base(&self) -> u64 {
        self.0
    }
}

const GUEST: u64 = 0x1000;
const REAL: u64 = 0x9000_0000;
const LEN: u64 = 0x1000;

/// One pool at `GUEST` of `LEN` bytes, whose buffer starts at `REAL`.
fn shifted() -> PoolSnapshot<Shifted> {
    PoolSnapshot::whole(GUEST, LEN, Shifted(REAL))
}

fn ubo(slot: u32, address: u64, size: u32, snapshot: Option<Arc<[u8]>>) -> UboDetail {
    UboDetail {
        stage: Stage::Compute,
        slot,
        address,
        size,
        snapshot,
    }
}

fn no_put(_: &Arc<[u8]>) -> Result<u64, Skip> {
    panic!("no snapshot expected");
}

#[test]
fn a_uniform_word_is_real_and_c0_stays_virtual() {
    let pools = shifted();
    let ubos = [ubo(2, GUEST + 0x40, 0x100, None)];
    let mut out = Vec::new();
    ubo_words(&mut out, &pools, Stage::Compute, &ubos, 0x77, &mut no_put).unwrap();
    assert_eq!(
        out,
        [UboWord {
            slot: 2,
            word: REAL + 0x40,
            guest: GUEST + 0x40,
            size: 0x100
        }]
    );
    let inp = StageInputs {
        stage: Stage::Compute,
        ubos: &ubos,
        sbufs: &[],
        textures: &[],
        images: &[],
        base_instance: 0,
        groups: Some([1, 1, 1]),
        push: None,
        blendz: 0,
    };
    let mut b = Vec::new();
    drv_block(&mut b, &inp, &out);
    let at = DRV_CS_UBO + 0x20;
    assert_eq!(&b[at..at + 8], &(GUEST + 0x40).to_le_bytes());
    assert_eq!(&b[at + 8..at + 12], &0x100u32.to_le_bytes());
}

#[test]
fn a_snapshot_keeps_its_scratch_address_and_its_guest_address() {
    let pools = shifted();
    let bytes: Arc<[u8]> = Arc::from(vec![1u8, 2, 3, 4]);
    let ubos = [ubo(0, GUEST + 8, 4, Some(bytes))];
    let mut out = Vec::new();
    let mut put = |b: &Arc<[u8]>| -> Result<u64, Skip> {
        assert_eq!(&**b, &[1, 2, 3, 4]);
        Ok(0x5555)
    };
    ubo_words(&mut out, &pools, Stage::Compute, &ubos, 0x77, &mut put).unwrap();
    assert_eq!((out[0].word, out[0].guest), (0x5555, GUEST + 8));
}

#[test]
fn a_uniform_outside_every_pool_reads_the_zero_buffer_and_is_counted() {
    let pools = shifted();
    let before = nxkit_nvn::diag::total(Reason::BindNoPool);
    let ubos = [
        ubo(1, GUEST + LEN + 0x10, 0x40, None),
        ubo(3, 0, 0x40, None),
    ];
    let mut out = Vec::new();
    ubo_words(&mut out, &pools, Stage::Compute, &ubos, 0x77, &mut no_put).unwrap();
    for (w, slot) in out.iter().zip([1, 3]) {
        assert_eq!(
            *w,
            UboWord {
                slot,
                word: 0x77,
                guest: 0,
                size: 0
            }
        );
    }
    assert!(nxkit_nvn::diag::total(Reason::BindNoPool) >= before + 2);
}

#[test]
fn a_uniform_longer_than_its_pool_is_clamped() {
    let pools = shifted();
    let ubos = [ubo(0, GUEST + LEN - 0x30, 0x100, None)];
    let mut out = Vec::new();
    ubo_words(&mut out, &pools, Stage::Compute, &ubos, 0x77, &mut no_put).unwrap();
    assert_eq!(out[0].word, REAL + LEN - 0x30);
    assert_eq!(out[0].size, 0x30);
}

#[test]
fn a_storage_buffer_word_is_real_or_zero() {
    let pools = shifted();
    let inside = ubo(0, GUEST + 0x20, 0x10, None);
    let outside = ubo(1, GUEST + 2 * LEN, 0x10, None);
    let word =
        |s: Option<&UboDetail>| sbuf_word(&pools, &NO_RENAME, s, true, 0x77, &mut no_put).unwrap();
    assert_eq!(word(Some(&inside)), REAL + 0x20);
    assert_eq!(word(Some(&outside)), 0x77);
    assert_eq!(word(None), 0x77);
    // a read-only buffer with a copy takes the copy, a writable one keeps the buffer
    let copy = ubo(0, GUEST + 0x20, 4, Some(Arc::from(vec![9u8; 4])));
    let mut put = |_: &Arc<[u8]>| -> Result<u64, Skip> { Ok(0x5555) };
    assert_eq!(
        sbuf_word(&pools, &NO_RENAME, Some(&copy), false, 0x77, &mut put).unwrap(),
        0x5555
    );
    assert_eq!(
        sbuf_word(&pools, &NO_RENAME, Some(&copy), true, 0x77, &mut no_put).unwrap(),
        REAL + 0x20
    );
}

#[test]
fn a_renamed_range_reads_its_copy_whatever_the_binding_says() {
    let pools = shifted();
    let map = crate::rename::RenameMap::from_entries(vec![(GUEST + 0x40, 0x40, 0x9000)]);
    let whole = ubo(0, GUEST + 0x40, 0x40, None);
    let inner = ubo(1, GUEST + 0x50, 4, Some(Arc::from(vec![9u8; 4])));
    let reaches = ubo(2, GUEST + 0x70, 0x20, None);
    for rw in [true, false] {
        assert_eq!(
            sbuf_word(&pools, &map, Some(&whole), rw, 0x77, &mut no_put).unwrap(),
            0x9000
        );
        // The same offset into the copy, and a snapshot of a read-only binding is not used.
        assert_eq!(
            sbuf_word(&pools, &map, Some(&inner), rw, 0x77, &mut no_put).unwrap(),
            0x9010
        );
    }
    // A binding that reaches out of the range keeps the buffer itself.
    assert_eq!(
        sbuf_word(&pools, &map, Some(&reaches), true, 0x77, &mut no_put).unwrap(),
        REAL + 0x70
    );
}

#[test]
fn the_identity_backing_gives_the_address_back() {
    let pools = PoolSnapshot::whole(GUEST, LEN, Shifted(GUEST));
    assert_eq!(locate(&pools, GUEST + 0x123, 8, 0), Some((GUEST + 0x123, 8)));
}

/// A sink that keeps what it gets.
struct Puts(Vec<Vec<u8>>);

impl Sink for Puts {
    fn put(&mut self, data: &[u8]) -> Result<u64, Skip> {
        self.0.push(data.to_vec());
        Ok(0x1000 + 0x10 * self.0.len() as u64)
    }
}

#[test]
fn the_blend_word_zero_binds_the_zero_buffer() {
    let mut sink = Puts(Vec::new());
    assert_eq!(blend_word(0, 0x77, &mut sink).unwrap(), 0x77);
    assert!(sink.0.is_empty());
}

#[test]
fn a_blend_word_puts_four_bytes() {
    let mut sink = Puts(Vec::new());
    let word = (2u32 << 3) | 1;
    let at = blend_word(word, 0x77, &mut sink).unwrap();
    assert_eq!(at, 0x1010);
    assert_eq!(sink.0, [word.to_le_bytes().to_vec()]);
}
