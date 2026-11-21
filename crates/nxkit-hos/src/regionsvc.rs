//! The observation gates of the SDK's address regions, answered by the kernel (see `regions`).

use crate::kernel::Kernel;
use crate::regions::LOGGED;

impl Kernel {
    /// The calling thread asks the SDK for an address region of `size` bytes (the gate at the entry of
    /// `VammManager::AllocateAddressRegion`, the size in `x2`).
    pub fn region_ask(&self, size: u64) {
        if let Some(vm) = self.vm.as_ref() {
            vm.regions.ask(size);
        }
    }

    /// The SDK made the region of the calling thread's last [`Kernel::region_ask`] at `addr` (the gate on the store
    /// through the out pointer). An address outside the alias region is not a region of it.
    pub fn region_got(&self, addr: u64) {
        let Some(vm) = self.vm.as_ref() else {
            return;
        };
        let (base, end) = (vm.layout.alias_base, vm.layout.alias_end);
        let inside = (base..end).contains(&addr);
        let size = vm.regions.got(inside.then(|| addr - base));
        if inside && size >= LOGGED {
            (self.config.log)(&format!(
                "[mem] address region {:.2} GiB at alias +{:.2} GiB",
                gib(size),
                gib(addr - base)
            ));
        }
    }

    /// The SDK freed the region at `addr` (the gate at the entry of `VammManager::FreeAddressRegion`).
    pub fn region_free(&self, addr: u64) {
        if let Some(vm) = self.vm.as_ref() {
            vm.regions.free(addr.wrapping_sub(vm.layout.alias_base));
        }
    }

    /// What the alias region has room for in one more address region: behind the regions made so far (at least the
    /// SDK's node area), less [`crate::ROOM_FOR_LATER`], in 32 MiB steps. 0 without a memory model.
    pub fn alias_room(&self) -> u64 {
        self.vm.as_ref().map_or(0, |vm| {
            vm.regions.room(vm.layout.alias_end - vm.layout.alias_base)
        })
    }

    /// The arena that the host keeps at the end of the alias region ([`crate::MemoryConfig::arena`]): `(address,
    /// size)`, or `None` when there is none.
    pub fn arena(&self) -> Option<(u64, u64)> {
        let vm = self.vm.as_ref()?;
        let size = vm.layout.end - vm.layout.arena_base;
        (size > 0).then_some((vm.layout.arena_base, size))
    }
}

fn gib(v: u64) -> f64 {
    v as f64 / f64::from(1u32 << 30)
}
