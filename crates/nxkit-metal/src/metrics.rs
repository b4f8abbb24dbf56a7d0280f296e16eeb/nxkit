//! Measurement: the CPU encode time and the GPU time of every command buffer.
//!
//! The cost is one `Instant` pair per command buffer and a few atomic additions, so it stays on.

use std::sync::atomic::{AtomicU64, Ordering};

/// The numbers of one command buffer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CommandBufferStats {
    /// The number of the frame (from 1).
    pub frame: u64,
    /// The CPU time from `FrameRing::begin` to the commit, in nanoseconds.
    pub cpu_encode_ns: u64,
    /// The GPU time from start to end of the command buffer, in nanoseconds.
    pub gpu_ns: u64,
    /// The GPU start time in seconds of the host clock (`CFTimeInterval`).
    pub gpu_start_s: f64,
    /// The GPU end time in seconds of the host clock (`CFTimeInterval`).
    pub gpu_end_s: f64,
    /// True when the GPU reported an error for this command buffer.
    pub failed: bool,
}

/// A copy of the counters at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MetricsSnapshot {
    /// The completed command buffers.
    pub completed: u64,
    /// The command buffers that ended with a GPU error.
    pub failed: u64,
    /// The sum of the CPU encode times, in nanoseconds.
    pub cpu_encode_total_ns: u64,
    /// The sum of the GPU times, in nanoseconds.
    pub gpu_total_ns: u64,
    /// The CPU encode time of the last command buffer.
    pub last_cpu_encode_ns: u64,
    /// The GPU time of the last command buffer.
    pub last_gpu_ns: u64,
    /// The longest GPU time so far.
    pub max_gpu_ns: u64,
}

impl MetricsSnapshot {
    /// The mean GPU time in nanoseconds, or 0 when nothing completed.
    pub fn mean_gpu_ns(&self) -> u64 {
        self.gpu_total_ns.checked_div(self.completed).unwrap_or(0)
    }

    /// The mean CPU encode time in nanoseconds, or 0 when nothing completed.
    pub fn mean_cpu_encode_ns(&self) -> u64 {
        self.cpu_encode_total_ns
            .checked_div(self.completed)
            .unwrap_or(0)
    }
}

/// The counters. Share one through an `Arc`.
#[derive(Debug, Default)]
pub struct Metrics {
    completed: AtomicU64,
    failed: AtomicU64,
    cpu_total: AtomicU64,
    gpu_total: AtomicU64,
    last_cpu: AtomicU64,
    last_gpu: AtomicU64,
    max_gpu: AtomicU64,
}

impl Metrics {
    /// Adds one command buffer.
    pub fn record(&self, s: &CommandBufferStats) {
        self.completed.fetch_add(1, Ordering::Relaxed);
        if s.failed {
            self.failed.fetch_add(1, Ordering::Relaxed);
        }
        self.cpu_total.fetch_add(s.cpu_encode_ns, Ordering::Relaxed);
        self.gpu_total.fetch_add(s.gpu_ns, Ordering::Relaxed);
        self.last_cpu.store(s.cpu_encode_ns, Ordering::Relaxed);
        self.last_gpu.store(s.gpu_ns, Ordering::Relaxed);
        self.max_gpu.fetch_max(s.gpu_ns, Ordering::Relaxed);
    }

    /// Reads the counters. The values are not one atomic snapshot, which is fine for a display.
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            completed: self.completed.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            cpu_encode_total_ns: self.cpu_total.load(Ordering::Relaxed),
            gpu_total_ns: self.gpu_total.load(Ordering::Relaxed),
            last_cpu_encode_ns: self.last_cpu.load(Ordering::Relaxed),
            last_gpu_ns: self.last_gpu.load(Ordering::Relaxed),
            max_gpu_ns: self.max_gpu.load(Ordering::Relaxed),
        }
    }
}

/// Hooks for frame pacing. The backend calls them. It does not pace by itself.
///
/// `frame_complete` runs on a Metal thread. Keep it short.
pub trait FrameHooks: Send + Sync {
    /// A frame starts encoding, after its slot is free.
    fn frame_begin(&self, _frame: u64) {}
    /// A frame is about to be presented.
    fn before_present(&self, _frame: u64) {}
    /// The GPU finished a frame.
    fn frame_complete(&self, _stats: &CommandBufferStats) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_and_means() {
        let m = Metrics::default();
        for (cpu, gpu) in [(10, 100), (30, 300)] {
            m.record(&CommandBufferStats {
                cpu_encode_ns: cpu,
                gpu_ns: gpu,
                ..Default::default()
            });
        }
        let s = m.snapshot();
        assert_eq!((s.completed, s.max_gpu_ns, s.last_gpu_ns), (2, 300, 300));
        assert_eq!((s.mean_gpu_ns(), s.mean_cpu_encode_ns()), (200, 20));
        assert_eq!(MetricsSnapshot::default().mean_gpu_ns(), 0);
    }
}
