//! `NXKIT_DIAG=pull` for a prepared draw. It reads guest memory through [`Shared`], so it sits with the draws and not
//! in `crate::pull`, which the draws use (W29).

use std::sync::atomic::{AtomicU64, Ordering};

use super::Shared;
use crate::detail::DrawDetail;
use crate::pull;

/// Pulled reads outside guest memory that `NXKIT_DIAG=pull` found, in total.
static STRAYS: AtomicU64 = AtomicU64::new(0);

/// `NXKIT_DIAG=pull`: checks that every element a pulled draw reads lies in guest memory, for the draws whose vertex
/// range the host knows (arrays, and indexed draws with readable indices), and prints the first reads that do not. A
/// read outside makes the GPU fault. The command buffer then fails (`nxkit-metal: warning: frame ... failed`). The
/// check works on guest addresses, so it builds its own block with the guest bases of the streams.
pub(crate) fn diag_draw(sh: &Shared<'_>, d: &DrawDetail, inputs: &[u32]) {
    let block = &pull::layout(d, Some);
    let indices = |index_type: u32, address: u64, count: u32| {
        let size = 1u64 << index_type.min(2);
        sh.guest_offset(address, u64::from(count) * size)?;
        let mut bytes = vec![0u8; (u64::from(count) * size) as usize];
        sh.read_guest(address, &mut bytes).then_some(())?;
        pull::index_range(index_type, &bytes)
    };
    let Some(ranges) = pull::model::ranges(d, indices) else {
        return;
    };
    let inside = |a: u64, n: u64| sh.guest_offset(a, n).is_some();
    if let Some(s) = pull::model::check(block, inputs, ranges, inside) {
        let n = STRAYS.fetch_add(1, Ordering::Relaxed) + 1;
        if n <= 32 {
            let m = sh.guest.address_map();
            eprintln!(
                "DIAG pull stray read {n}: vs {} location {} reads {} bytes at {:#x}, guest {:#x}+{:#x}, vertices {:?} \
                 instances {:?} args {:?} streams {:?} attributes {:?}",
                d.program.vertex,
                s.location,
                s.element.bytes,
                s.element.address,
                m.gpu_base(),
                m.len(),
                ranges.0,
                ranges.1,
                d.args,
                d.streams,
                d.attributes
            );
        }
    }
}
