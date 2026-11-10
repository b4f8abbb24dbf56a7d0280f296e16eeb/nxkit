use super::*;
use crate::rename::{CapRange, Kind};
use nxkit_metal::{ArchiveConfig, Context, DataBuffer, FrameRing};

#[test]
fn the_job_bytes_follow_the_layout_of_the_kernels() {
    let b = job_bytes(0x10, 0x20, 0x30, 0x40, 0x50, 7);
    let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    assert_eq!(
        (u64_at(0), u64_at(8), u64_at(16), u64_at(24), u64_at(32)),
        (0x10, 0x20, 0x30, 0x40, 0x50)
    );
    assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 7);
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
    let mut captured: Vec<u32> = (500..540).collect();
    captured[1] = 11;
    captured[33] = 33;
    let start: Vec<u8> = captured.iter().flat_map(|v| v.to_le_bytes()).collect();
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
            start,
            kind: Kind::Merge,
            cmask: mask(&[1, 33]),
            gmask: mask(&[2, 35]),
        }],
    });
    let runs = [MergeRun {
        cap,
        idx: 0,
        guest: guest.gpu_address(),
        slot: slot.gpu_address(),
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
