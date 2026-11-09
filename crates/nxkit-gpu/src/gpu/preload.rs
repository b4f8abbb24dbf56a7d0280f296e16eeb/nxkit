//! The preload thread: the uniform snapshots of a frame go to the scratch memory once, before the chunks start.

use std::thread::JoinHandle;

use super::*;

/// The scratch address of each uniform snapshot, by the address of its bytes.
pub(super) type Preloaded = FastMap<usize, u64>;

/// Starts the thread that pushes every uniform snapshot of `detail` to `scratch`.
pub(super) fn start(
    detail: &FrameDetail,
    scratch: &Arc<Scratch>,
) -> std::io::Result<JoinHandle<Preloaded>> {
    let snaps: Vec<Arc<[u8]>> = detail
        .draws
        .iter()
        .flat_map(|d| d.ubos.iter().chain(d.sbufs.iter()))
        .chain(detail.dispatches.iter().flat_map(|d| d.ubos.iter()))
        .filter_map(|u| u.snapshot.clone())
        .collect();
    let scratch = scratch.clone();
    std::thread::Builder::new()
        .name("nxkit-preload".into())
        .spawn(move || {
            // Every uniform snapshot of the frame goes to the scratch memory once. The chunks then only look the
            // addresses up; when each chunk uploaded on first use, the shared map was locked twice per uniform of
            // every draw by all chunk threads.
            let mut preloaded: Preloaded = FastMap::default();
            for bytes in &snaps {
                let key = bytes.as_ptr() as usize;
                if !preloaded.contains_key(&key)
                    && let Ok(a) = scratch.push(bytes)
                {
                    preloaded.insert(key, a);
                }
            }
            preloaded
        })
}

/// The addresses the thread collected, or none when it did not start or panicked.
pub(super) fn finish(thread: std::io::Result<JoinHandle<Preloaded>>) -> Preloaded {
    match thread {
        Ok(h) => h.join().unwrap_or_default(),
        Err(_) => FastMap::default(),
    }
}
