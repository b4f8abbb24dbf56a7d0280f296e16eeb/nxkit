//! The observation gates of the rewritten code (R46): their ids and the handler a subsystem gives for one.
//!
//! `nxb-rewrite` writes an `obs` row in a module's gates file for each observation gate, with an argument whose low 8
//! bits are the id and whose upper bits are a parameter of the handler (often a register number). The ids are the
//! numbering of the old engine's `dm.h` (`OBS_*`), which `nxb-rewrite`'s `obs.rs` and `engine.rs` keep. The host calls
//! the handler of the id with the guest's registers x0 to x28 that the gate saved; what the handler writes there takes
//! effect when the gate returns (x18 excepted, which the gate does not load back).

/// The cut FMallocBinned3 constructor call of Unreal (`ue_malloc`); the parameter is the register of the object.
pub const UE_MALLOC: u8 = 21;
/// The entry of the SDK's `VammManager::AllocateAddressRegion`; the size is in x2.
pub const REGION_ASK: u8 = 22;
/// The store of the new region's address through the out pointer; the parameter is the register of the address.
pub const REGION_GOT: u8 = 23;
/// The entry of `VammManager::FreeAddressRegion`; the address is in x1.
pub const REGION_FREE: u8 = 24;
/// The entry of `StandardAllocator::Initialize`.
pub const STDALLOC_INIT: u8 = 25;
/// The store of the size of Unreal's engine region before `nninitStartup` reserves it (`ue_region`); the parameter is
/// the register of the size.
pub const UE_REGION: u8 = 26;

/// The words a handler gets: x0 to x28.
pub const REGS: usize = 29;

/// The handler of one id: the registers x0 to x28 and the parameter of the gate.
pub type Handler = Box<dyn Fn(&mut [u64], u32) + Send + Sync>;

/// The id and the parameter of a gate's argument.
pub fn split(arg: u32) -> (u8, u32) {
    ((arg & 0xff) as u8, arg >> 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_argument_is_the_id_and_a_parameter() {
        assert_eq!(split(282), (UE_REGION, 1));
        assert_eq!(split(5655), (REGION_GOT, 22));
        assert_eq!(split(21), (UE_MALLOC, 0));
    }
}
