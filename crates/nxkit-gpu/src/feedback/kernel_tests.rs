//! The host kernels of the captures on the GPU.

use nxkit_metal::{ArgumentTable, ComputeCommand, Context, DataBuffer};

use super::run::{MSL, job_bytes};
use crate::pipelines::Pipelines;

/// Byte offsets in the test buffer.
const JOB: u64 = 0;
const STATE: u64 = 256;
const IB: u64 = 512;
const ORDER: u64 = 1024;
const TF: u64 = 2048;
const DRAW_TF: u64 = 2112;
const DRAW_VS: u64 = 2128;
const SAVE: u64 = 2176;

fn words(b: &DataBuffer, at: u64, n: usize) -> Vec<u32> {
    let mut out = vec![0u8; n * 4];
    b.read(at, &mut out).unwrap();
    out.chunks(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn put_words(b: &DataBuffer, at: u64, w: &[u32]) {
    let bytes: Vec<u8> = w.iter().flat_map(|v| v.to_le_bytes()).collect();
    b.write(at, &bytes).unwrap();
}

/// Runs `fb_finish` once over the job at the start of `b`.
fn finish(ctx: &Context, b: &DataBuffer) {
    let pipelines = Pipelines::new(ctx, &nxkit_metal::ArchiveConfig::default()).unwrap();
    let pipeline = pipelines
        .host_compute("nxkit-feedback", MSL, "fb_finish")
        .unwrap();
    let table = ArgumentTable::new(ctx, 31, 0).unwrap();
    let mut ring = nxkit_metal::BatchRing::new(ctx, 1).unwrap();
    let mut batch = ring.begin(1).unwrap();
    {
        let cb = &mut batch.buffers()[0];
        let mut cp = cb.begin_compute_pass(&table).unwrap();
        for c in [
            ComputeCommand::SetPipeline(pipeline),
            ComputeCommand::SetBuffer {
                index: 0,
                gpu_addr: b.gpu_address() + JOB,
            },
            ComputeCommand::DispatchThreadgroups {
                groups: [1, 1, 1],
                threads: [1, 1, 1],
            },
        ] {
            cp.encode(&c).unwrap();
        }
        cp.end();
    }
    ctx.commit_residency();
    batch.submit().unwrap().wait().unwrap();
}

#[test]
fn a_geometry_capture_takes_the_emitted_points_in_order_up_to_the_room_of_its_buffer() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let b = DataBuffer::new(&ctx, 4096).unwrap();
    let a = |o: u64| b.gpu_address() + o;
    // Two input points, one invocation, index lists of 4: the first emits vertex 5, the second 7 and 9.
    const R: u32 = 0xffff_ffff;
    put_words(&b, IB, &[5, R, R, R, 7, 9, R, R]);
    put_words(&b, DRAW_VS, &[2, 1, 0, 0]);
    let job = job_bytes(
        [
            a(STATE),
            0,
            0,
            a(SAVE),
            0,
            0,
            a(DRAW_VS),
            0,
            a(IB),
            a(ORDER),
            a(TF),
            a(DRAW_TF),
        ],
        [52, 0, 0, 0],
        // Room for two records of 52 bytes.
        [104, 0, 0, 0],
        [1, 0, 0, 1, 0, 2, 1, 1, 4, 1, 32, 0],
    );
    b.write(JOB, &job).unwrap();
    finish(&ctx, &b);
    assert_eq!(&words(&b, ORDER, 3)[..2], &[5, 7], "the order of the lists");
    assert_eq!(words(&b, DRAW_TF, 4), [2, 1, 0, 0], "two records fit");
    let tf = words(&b, TF, 12);
    assert_eq!(&tf[0..8], &[0, 0, 0, 0, 2, 1, 0, 0], "offsets, then args");
    assert_eq!(tf[11], 2, "count");
    assert_eq!(
        words(&b, SAVE, 8),
        [104, 0, 0, 0, 2, 1, 0, 0],
        "the saved control state"
    );
}

#[test]
fn a_vertex_capture_draws_the_vertices_of_its_draw_and_goes_on_from_a_loaded_state() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let b = DataBuffer::new(&ctx, 4096).unwrap();
    let a = |o: u64| b.gpu_address() + o;
    // A resume: the state comes from the control block at SAVE, 32 bytes of buffer 0 used and 2 records.
    put_words(&b, SAVE, &[32, 0, 0, 0, 2, 1, 0, 0]);
    put_words(&b, DRAW_VS, &[4, 1, 3, 0]);
    let job = job_bytes(
        [
            a(STATE),
            a(SAVE),
            0,
            a(SAVE),
            0,
            0,
            a(DRAW_VS),
            0,
            0,
            0,
            a(TF),
            a(DRAW_TF),
        ],
        [16, 0, 0, 0],
        [1024, 0, 0, 0],
        [0, 4, 3, 1, 0, 4, 1, 0, 0, 0, 1, 0],
    );
    b.write(JOB, &job).unwrap();
    finish(&ctx, &b);
    assert_eq!(
        words(&b, DRAW_TF, 4),
        [4, 1, 3, 0],
        "the draw's own vertices"
    );
    let tf = words(&b, TF, 12);
    assert_eq!(tf[0], 32, "the records go on after the loaded ones");
    assert_eq!(tf[4], 4, "four records");
    assert_eq!((tf[10], tf[11]), (3, 4), "first and count of the draw");
    assert_eq!(words(&b, SAVE, 8), [96, 0, 0, 0, 6, 1, 0, 0]);
}
