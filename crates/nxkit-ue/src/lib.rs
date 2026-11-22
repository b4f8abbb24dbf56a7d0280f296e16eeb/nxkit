//! The Unreal Engine take-overs of the nxkit host: the engine heap and the size of the engine region.
//!
//! Unreal Engine 4 and 5 on the Switch reserve one address region for the engine in `nninitStartup`, 62 GiB by
//! default, made for the Switch's 64 GiB alias region. FMallocBinned3 reserves 37 GiB of address space for its pools
//! inside it (74 pools of 512 MiB). The GPU buffer of nxkit covers the whole guest block, and the driver wires every
//! page of it, so the alias region is a few GiB and neither fits. The rewriter's engine-wide rules (`ue_region`,
//! `ue_malloc` in `nxb-rewrite`) put an observation gate on the store of the region's size and cut the one call of
//! the FMallocBinned3 constructor, with a gate on it and the guest's vtable and its layout in the fixups table.
//!
//! - [`cap_region`] gives the engine region what the alias region has room for (the kernel's answer).
//! - [`install`] makes a [`heap::Heap`] over the arena the kernel keeps at the end of the alias region, builds a
//!   vtable of the guest's layout whose allocation slots are this crate's functions, and returns the handler of the
//!   allocator gate, which puts that vtable into the object the engine made. Every `FMemory` allocation then reaches
//!   the heap. Its memory lies in the guest block, so it is GPU visible (NVN pools the engine allocates through it)
//!   and the kernel's `MapMemory` takes a thread stack from it.
//!
//! - [`wire`] is what the host wires: the size of the arena ([`wire::arena_size`]) and the handlers of the
//!   observation gates by id ([`wire::handlers`]). The host passes the fixups, the kernel's answers and
//!   [`settings::UeSettings`].
//!
//! The functions of the vtable are called by guest code with no Rust context, so the heap is a process-wide value
//! that [`install`] sets once.

pub mod heap;
mod layout;
pub mod settings;
mod vt;
pub mod wire;

pub use layout::{LAYOUTS, Layout, layout_for_name_slot};
pub use vt::{Installed, install};

/// The engine region asks for `regs[param]` bytes (and `x1`, the request of `AllocateAddressRegion`). When that is
/// more than `room`, both registers get `room`. Returns `(asked, given)` when it capped them. `param` names a
/// register x1 to x28 other than x18. Any other leaves the registers alone.
pub fn cap_region(regs: &mut [u64], param: u32, room: u64) -> Option<(u64, u64)> {
    let p = param as usize;
    if !(1..=28).contains(&p) || p == 18 || regs.len() <= p {
        return None;
    }
    let asked = regs[p];
    if room == 0 || asked <= room {
        return None;
    }
    regs[p] = room;
    regs[1] = room;
    eprintln!(
        "nxkit-ue: [mem] Unreal's engine region: {:.2} GiB asked, {:.2} GiB given, what the alias region has room for",
        gib(asked),
        gib(room)
    );
    Some((asked, room))
}

fn gib(v: u64) -> f64 {
    v as f64 / f64::from(1u32 << 30)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_region_is_capped_to_the_room_in_its_register_and_x1() {
        let mut r = [0u64; 29];
        r[1] = 62 << 30;
        r[8] = 62 << 30;
        assert_eq!(cap_region(&mut r, 8, 3 << 30), Some((62 << 30, 3 << 30)));
        assert_eq!((r[1], r[8]), (3 << 30, 3 << 30));
        // a request that fits stays
        assert_eq!(cap_region(&mut r, 8, 4 << 30), None);
        assert_eq!(r[8], 3 << 30);
        // no room or a bad register: nothing changes
        r[8] = 62 << 30;
        assert_eq!(cap_region(&mut r, 8, 0), None);
        assert_eq!(cap_region(&mut r, 18, 1 << 30), None);
        assert_eq!(cap_region(&mut r, 29, 1 << 30), None);
        assert_eq!(r[8], 62 << 30);
    }
}
