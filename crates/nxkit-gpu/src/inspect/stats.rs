//! The numbers of the last frames for the `stats` verb: the time between two frames, the encode time, the GPU time,
//! the draws, the pipeline builds and the textures that the encoder made. The encoder notes each frame at its end.
//! The note is one lock and a few stores.

use std::sync::Mutex;
use std::time::Instant;

use crate::stats::EncodeStats;

/// The frames that the ring keeps.
const KEEP: usize = 120;

/// The numbers of one frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Sample {
    pub(crate) frame: u64,
    /// When the encoder finished the frame.
    pub(crate) at: Instant,
    pub(crate) encode_ns: u64,
    pub(crate) draws: u32,
    pub(crate) render_passes: u32,
    pub(crate) pipeline_builds: u32,
    pub(crate) textures_created: u32,
    /// The sum of the GPU times of all command buffers that had completed until now.
    pub(crate) gpu_total_ns: u64,
}

#[derive(Default)]
struct Ring {
    samples: Vec<Sample>,
    builds_total: u64,
    textures_total: u64,
}

/// The frames of the ring, written by the encoder and read by the `stats` verb. A counter log of the diagnostics (W33).
static RING: Mutex<Ring> = Mutex::new(Ring {
    samples: Vec::new(),
    builds_total: 0,
    textures_total: 0,
});

/// Notes a frame that the encoder finished. `gpu_total_ns` is the sum of the GPU times that Metal reported so far.
pub(crate) fn note_frame(frame: u64, stats: &EncodeStats, gpu_total_ns: u64) {
    let sample = Sample {
        frame,
        at: Instant::now(),
        encode_ns: stats.encode_ns,
        draws: stats.draws,
        render_passes: stats.render_passes,
        pipeline_builds: stats.pipeline_builds,
        textures_created: stats.textures_created,
        gpu_total_ns,
    };
    let mut ring = RING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ring.builds_total += u64::from(sample.pipeline_builds);
    ring.textures_total += u64::from(sample.textures_created);
    if ring.samples.len() == KEEP {
        ring.samples.remove(0);
    }
    ring.samples.push(sample);
}

/// The lines of the `stats` verb.
pub(crate) fn lines() -> Vec<String> {
    let ring = RING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    describe(&ring.samples, ring.builds_total, ring.textures_total)
}

fn ms(ns: f64) -> f64 {
    ns / 1e6
}

/// Mean, 95th percentile and maximum of `v` (not empty).
fn spread(v: &mut [f64]) -> (f64, f64, f64) {
    v.sort_by(f64::total_cmp);
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    let p95 = v[((v.len() - 1) as f64 * 0.95).round() as usize];
    (mean, p95, v[v.len() - 1])
}

fn describe(s: &[Sample], builds_total: u64, textures_total: u64) -> Vec<String> {
    let (Some(first), Some(last)) = (s.first(), s.last()) else {
        return vec!["no frame was encoded yet".to_owned()];
    };
    let n = s.len();
    let mut out = vec![format!(
        "frames {n} (frame {} to {}), {:.1} s",
        first.frame,
        last.frame,
        last.at.duration_since(first.at).as_secs_f64()
    )];
    if n > 1 {
        let mut gaps: Vec<f64> = s
            .windows(2)
            .map(|w| ms(w[1].at.duration_since(w[0].at).as_nanos() as f64))
            .collect();
        let (mean, p95, max) = spread(&mut gaps);
        out.push(format!(
            "frame time ms: mean {mean:.2} p95 {p95:.2} max {max:.2} ({:.1} fps)",
            1000.0 / mean.max(1e-9)
        ));
    }
    let mut enc: Vec<f64> = s.iter().map(|x| ms(x.encode_ns as f64)).collect();
    let (mean, p95, max) = spread(&mut enc);
    out.push(format!(
        "encode ms: mean {mean:.2} p95 {p95:.2} max {max:.2}"
    ));
    if n > 1 {
        // The GPU time that completed between the first and the last note, over the frames between them.
        let gpu = last.gpu_total_ns.saturating_sub(first.gpu_total_ns) as f64 / (n - 1) as f64;
        out.push(format!(
            "gpu ms per frame: mean {:.2} (command buffer times, a frame behind)",
            ms(gpu)
        ));
    }
    let mean = |f: fn(&Sample) -> u32| s.iter().map(|x| f64::from(f(x))).sum::<f64>() / n as f64;
    out.push(format!(
        "draws per frame: mean {:.0}, render passes per frame: mean {:.0}",
        mean(|x| x.draws),
        mean(|x| x.render_passes)
    ));
    let sum = |f: fn(&Sample) -> u32| s.iter().map(|x| u64::from(f(x))).sum::<u64>();
    out.push(format!(
        "pipeline builds: {} in these frames, {builds_total} since the start",
        sum(|x| x.pipeline_builds)
    ));
    out.push(format!(
        "textures created: {} in these frames, {textures_total} since the start",
        sum(|x| x.textures_created)
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sample(frame: u64, at: Instant, encode_ms: u64, gpu_total_ms: u64) -> Sample {
        Sample {
            frame,
            at,
            encode_ns: encode_ms * 1_000_000,
            draws: 100,
            render_passes: 10,
            pipeline_builds: u32::from(frame == 2),
            textures_created: 0,
            gpu_total_ns: gpu_total_ms * 1_000_000,
        }
    }

    #[test]
    fn no_frame_says_so() {
        assert_eq!(describe(&[], 0, 0), ["no frame was encoded yet"]);
    }

    #[test]
    fn the_lines_hold_the_means_and_the_counts() {
        let t0 = Instant::now();
        let s: Vec<Sample> = (0..5)
            .map(|i| sample(i + 1, t0 + Duration::from_millis(20 * i), 2 + i, 10 * i))
            .collect();
        let lines = describe(&s, 7, 9);
        assert_eq!(lines[0], "frames 5 (frame 1 to 5), 0.1 s");
        assert!(
            lines[1].starts_with("frame time ms: mean 20.00 p95 20.00 max 20.00 (50.0 fps)"),
            "{}",
            lines[1]
        );
        assert!(lines[2].starts_with("encode ms: mean 4.00"), "{}", lines[2]);
        assert!(
            lines[3].starts_with("gpu ms per frame: mean 10.00"),
            "{}",
            lines[3]
        );
        assert!(
            lines[4].starts_with("draws per frame: mean 100, render passes per frame: mean 10")
        );
        assert_eq!(
            lines[5],
            "pipeline builds: 1 in these frames, 7 since the start"
        );
        assert_eq!(
            lines[6],
            "textures created: 0 in these frames, 9 since the start"
        );
    }

    #[test]
    fn one_frame_has_no_intervals() {
        let s = [sample(1, Instant::now(), 3, 0)];
        let lines = describe(&s, 0, 0);
        assert!(lines.iter().all(|l| !l.starts_with("frame time")));
        assert!(lines.iter().all(|l| !l.starts_with("gpu ms")));
    }
}
