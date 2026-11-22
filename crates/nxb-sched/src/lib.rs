//! The executor (layer L2): a work-stealing scheduler with a token budget in P-core equivalents, priorities,
//! DAG joins and single-flight by content key. It runs generic tasks and knows no shaders.
//!
//! - [`Budget`]: tokens from P and E core counts (`round(P + e·E) + oversubscription`).
//! - [`Executor`] / [`Task`] / [`Ctx`]: worker threads, a priority injector, per-worker deques with stealing.
//! - [`Join`]: run a task after `n` predecessors.
//! - [`UseManifest`] and [`Partial`]: manifest-first ordering and the marker of a partial build.
//! - [`SingleFlight`]: equal work runs once, and followers continue without blocking a worker.
//! - [`Headroom`]: admits child runs by machine load (CPU, memory, memory pressure) between a floor and a ceiling.
//!
//! Std only, `std::thread` and channels, no async runtime (design-v2 8.5). Planned: more resource kinds (`mem` bytes,
//! `io.read(dev)`, `io.write(dev)`, `meta(dev)`, design-pipeline-rest 7), priorities from store history (LPT), child
//! CPU from `rusage` in the utilisation metric, QoS classes through `nxb-sys`.

mod budget;
mod executor;
mod headroom;
mod join;
mod manifest;
mod single_flight;

pub use budget::{Budget, DEFAULT_E_FACTOR};
pub use executor::{CancelHandle, Ctx, Executor, Report, Task};
pub use headroom::{Gauge, Headroom, Limits, Permit, Sample, SystemGauge, room};
pub use join::Join;
pub use manifest::{PARTIAL_FILE, Partial, UseManifest};
pub use single_flight::{Claim, Leader, SingleFlight};

#[cfg(test)]
mod tests;
