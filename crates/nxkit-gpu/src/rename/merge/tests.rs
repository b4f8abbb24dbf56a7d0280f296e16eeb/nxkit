use super::*;
use crate::rename::{CapRange, Kind};
use nxkit_metal::{ArchiveConfig, Context, DataBuffer, FrameRing};

#[test]
fn the_job_bytes_follow_the_layout_of_the_kernels() {
    let b = job_bytes([0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70], 7);
    let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    assert_eq!(
        (u64_at(0), u64_at(8), u64_at(16), u64_at(24), u64_at(32)),
        (0x10, 0x20, 0x30, 0x40, 0x50)
    );
    assert_eq!((u64_at(40), u64_at(48)), (0x60, 0x70));
    assert_eq!(u32::from_le_bytes(b[56..60].try_into().unwrap()), 7);
}

fn words(b: &DataBuffer, n: usize) -> Vec<u32> {
    let mut bytes = vec![0u8; 4 * n];
    b.read(0, &mut bytes).unwrap();
    bytes
        .chunks(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn put(b: &DataBuffer, w: &[u32]) {
    let bytes: Vec<u8> = w.iter().flat_map(|v| v.to_le_bytes()).collect();
    b.write(0, &bytes).unwrap();
}

/// Runs the two kernels on the GPU: the copy takes the guest words except the words of the CPU, which come from the
/// capture, and the words of the GPU go back.
#[test]
fn the_kernels_fill_the_copy_and_merge_the_gpu_words_back() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let Ok(pipelines) = Pipelines::new(&ctx, &ArchiveConfig::default()) else {
        return;
    };
    let guest = DataBuffer::new(&ctx, 256).unwrap();
    let slot = DataBuffer::new(&ctx, 256).unwrap();
    // 40 words. The GPU's state is in words 2 and 35, the CPU writes words 1 and 33. The guest holds a stale value of the
    // CPU's words and the state of the GPU.
    let mut g: Vec<u32> = (100..140).collect();
    g[1] = 0xdead;
    g[33] = 0xbeef;
    put(&guest, &g);
    // The CPU's words, packed: words 1 and 33. Block 0 has none before it, block 1 has one.
    let mask = |ws: &[usize]| {
        let mut m = vec![0u32; 2];
        for w in ws {
            m[w / 32] |= 1 << (w % 32);
        }
        m
    };
    let cap = Arc::new(Capture {
        fence: 1,
        sampled: false,
        epoch: 0,
        ranges: vec![CapRange {
            addr: 0x1000,
            len: 160,
            start: Vec::new(),
            kind: Kind::Merge,
            cmask: mask(&[1, 33]),
            gmask: mask(&[2, 35]),
            rmask: vec![0; 2],
            cvals: vec![11, 33],
            cprefix: vec![0, 1],
        }],
    });
    let runs = [MergeRun {
        cap,
        idx: 0,
        guest: guest.gpu_address(),
        slot: slot.gpu_address(),
        copy2: 0,
    }];
    let scratch = Scratch::new(&ctx, 1 << 16).unwrap();
    let table = ArgumentTable::new(&ctx, 31, 0).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    encode(
        &pipelines,
        frame.commands(),
        &table,
        &scratch,
        &runs,
        Phase::In,
    )
    .unwrap();
    ctx.commit_residency();
    frame.submit().unwrap().wait().unwrap();
    let copy = words(&slot, 40);
    // Words of the CPU come from the capture, the others from the guest.
    assert_eq!(copy[1], 11);
    assert_eq!(copy[33], 33);
    assert_eq!(copy[2], 102);
    assert_eq!(copy[35], 135);
    assert_eq!(copy[0], 100);
    // The frame changes the copy: its word 2 (GPU state) and word 5 (not in a mask).
    let mut c2 = copy.clone();
    c2[2] = 4242;
    c2[5] = 9999;
    put(&slot, &c2);
    let mut frame = ring.begin().unwrap();
    encode(
        &pipelines,
        frame.commands(),
        &table,
        &scratch,
        &runs,
        Phase::Out,
    )
    .unwrap();
    ctx.commit_residency();
    frame.submit().unwrap().wait().unwrap();
    let after = words(&guest, 40);
    assert_eq!(after[2], 4242, "the GPU's word goes back");
    assert_eq!(after[5], 105, "a word outside the masks stays");
    assert_eq!(after[1], 0xdead, "the CPU's word is not written by the GPU");
    assert_eq!(after[33], 0xbeef);
}

/// A probe: the copy and the copy of the start state are filled from guest memory; at the end every word that differs from
/// the start state goes back, and no other.
#[test]
fn a_probe_writes_back_every_word_that_the_gpu_changed_and_only_those() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let Ok(pipelines) = Pipelines::new(&ctx, &ArchiveConfig::default()) else {
        return;
    };
    let guest = DataBuffer::new(&ctx, 256).unwrap();
    let slot = DataBuffer::new(&ctx, 512).unwrap();
    let g: Vec<u32> = (100..140).collect();
    put(&guest, &g);
    let cap = Arc::new(Capture {
        fence: 1,
        sampled: false,
        epoch: 0,
        ranges: vec![CapRange {
            addr: 0x1000,
            len: 160,
            start: Vec::new(),
            kind: Kind::Probe,
            cmask: vec![0; 2],
            gmask: Vec::new(),
            rmask: Vec::new(),
            cvals: Vec::new(),
            cprefix: vec![0; 2],
        }],
    });
    // The copy at 0, the copy of the start state at 256.
    let runs = [MergeRun {
        cap,
        idx: 0,
        guest: guest.gpu_address(),
        slot: slot.gpu_address(),
        copy2: slot.gpu_address() + 256,
    }];
    let scratch = Scratch::new(&ctx, 1 << 16).unwrap();
    let table = ArgumentTable::new(&ctx, 31, 0).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    encode(
        &pipelines,
        frame.commands(),
        &table,
        &scratch,
        &runs,
        Phase::In,
    )
    .unwrap();
    ctx.commit_residency();
    frame.submit().unwrap().wait().unwrap();
    let both = words(&slot, 128);
    assert_eq!(&both[..40], &g[..]);
    assert_eq!(&both[64..104], &g[..]);
    // The GPU changes words 3 and 20 of the copy; the CPU changes word 7 of guest memory meanwhile.
    let mut c = both[..40].to_vec();
    c[3] = 7777;
    c[20] = 8888;
    let mut all = both.clone();
    all[..40].copy_from_slice(&c);
    put(&slot, &all);
    let mut g2 = g.clone();
    g2[7] = 4711;
    put(&guest, &g2);
    let mut frame = ring.begin().unwrap();
    encode(
        &pipelines,
        frame.commands(),
        &table,
        &scratch,
        &runs,
        Phase::Out,
    )
    .unwrap();
    ctx.commit_residency();
    frame.submit().unwrap().wait().unwrap();
    let after = words(&guest, 40);
    assert_eq!((after[3], after[20]), (7777, 8888));
    assert_eq!(after[7], 4711, "the CPU's word stays");
    assert_eq!(after[0], 100);
}
