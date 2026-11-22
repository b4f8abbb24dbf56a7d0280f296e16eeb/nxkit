//! The guest ABI helpers of the Vulkan functions.
//!
//! Argument conventions (they come from the rules in `lib.rs` and `vulkan_core.h`):
//! - A handle, a pointer, a `VkDeviceSize` and a `size_t` are `u64`. A guest address is a host address (R18).
//! - A `uint32_t`, a flags value, an enum and a `VkBool32` are `u32`. An `int32_t` is `i32`.
//! - A `float` is `f32`: AArch64 passes it in a SIMD register, so it must not be declared as an integer.
//! - A `VkResult` is returned as `i32`. No function is variadic.
//!
//! Errors (R15). A `VkError::Fault` (a struct of the game outside guest memory) and `VkError::Unknown` (a handle that
//! names no live object, the only source of that code in `nxkit-vk`) end the game with the name of the function
//! ([`fatal`]). Every other error is a `VkResult` code for the game. A function that returns `void` has no code to
//! give, so every error of it ends the game.

use std::fmt;

use nxkit_vk::Vk;
use nxkit_vk::mem::{GuestMemory, Mem};
use nxkit_vk::result::{Status, VkError, VkResult, code_of};

/// Guest memory of the running game, through `nxkit_abi::guest`.
pub(crate) const M: GuestMemory = GuestMemory;

/// Ends the game and says which Vulkan function did it. The process aborts, as an NVN failure does (R15).
pub(crate) fn fatal(name: &str, why: impl fmt::Display) -> ! {
    eprintln!("nxkit-rt: warning: Vulkan {name}: {why}");
    std::process::abort()
}

/// Whether an error ends the game instead of going back as a code: a guest memory fault or an unknown handle.
pub(crate) fn ends_game(e: VkError) -> bool {
    matches!(e, VkError::Fault { .. } | VkError::Unknown)
}

/// What the game gets for the result of a body: the code, or the end of the game for a fatal error.
pub(crate) fn verdict(name: &str, r: VkResult<Status>) -> i32 {
    match r {
        Err(e) if ends_game(e) => fatal(name, e),
        r => code_of(r),
    }
}

/// Runs the body of a function that returns a `VkResult`, with the installed front end.
pub(crate) fn code(name: &str, body: impl FnOnce(&Vk) -> VkResult<Status>) -> i32 {
    verdict(name, body(&super::ctx::ctx(name).vk))
}

/// Runs the body of a function that returns `void`. Any error ends the game.
pub(crate) fn call(name: &str, body: impl FnOnce(&Vk) -> VkResult<()>) {
    if let Err(e) = body(&super::ctx::ctx(name).vk) {
        fatal(name, e)
    }
}

/// Writes a new handle to the game's `out` pointer.
pub(crate) fn handle_out(out: u64, h: VkResult<u64>) -> VkResult<Status> {
    M.put_u64(out, h?)?;
    Ok(Status::Success)
}

/// Writes new handles to the game's array at `out`.
pub(crate) fn handles_out(out: u64, hs: VkResult<Vec<u64>>) -> VkResult<Status> {
    let bytes: Vec<u8> = hs?.iter().flat_map(|h| h.to_le_bytes()).collect();
    if !bytes.is_empty() {
        M.write(out, &bytes)?;
    }
    Ok(Status::Success)
}

/// A result without a value as `VK_SUCCESS`.
pub(crate) fn ok(r: VkResult<()>) -> VkResult<Status> {
    r.map(|()| Status::Success)
}

/// Builds the procs of a group: `procs!["vkFoo" => vk_foo, "vkBar" => vk_bar [big]]`. `[big]` marks a function that
/// runs on the big host stack.
macro_rules! procs {
    (@big) => {
        false
    };
    (@big big) => {
        true
    };
    ($($name:literal => $f:ident $([$big:ident])?),* $(,)?) => {
        vec![$($crate::vk::Proc {
            name: $name,
            addr: $f as *const () as usize,
            big: procs!(@big $($big)?),
        }),*]
    };
}
pub(crate) use procs;
