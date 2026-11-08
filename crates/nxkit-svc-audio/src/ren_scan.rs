//! What the host reads off the SDK's own machine code: the update lock of the renderer system and the place of the system
//! inside the renderer object. This follows `lock_in`, `update_lock` and `impl_sys_off` of nxnative.
//!
//! The SDK versions differ in these offsets, and a wrong guess excludes nothing. The scan reads the first instructions of
//! an SDK function, as nxnative does. It has not run on a game yet.

use crate::host::sym_opt;
use crate::sys::call::{read_insn, read_ptr};
use std::sync::atomic::{AtomicUsize, Ordering};

/// `nn::os::LockMutex(MutexType*)`.
pub(crate) const LOCK_MUTEX: &str = "_ZN2nn2os9LockMutexEPNS0_9MutexTypeE";
/// `nn::os::UnlockMutex(MutexType*)`.
pub(crate) const UNLOCK_MUTEX: &str = "_ZN2nn2os11UnlockMutexEPNS0_9MutexTypeE";
/// `nn::os::SdkMutexType::Lock()`.
pub(crate) const SDK_LOCK: &str = "_ZN2nn2os12SdkMutexType4LockEv";
/// `nn::os::SdkMutexType::Unlock()`.
pub(crate) const SDK_UNLOCK: &str = "_ZN2nn2os12SdkMutexType6UnlockEv";
const UPDATE: &str = "_ZN2nn5audio6server17AudioRenderSystem6UpdateEPvmS3_mPKvm";
const UPDATE_IMPL: &str = "_ZN2nn5audio6server17AudioRenderSystem10UpdateImplEPvmS3_mPKvm";
/// `AudioRendererImpl::ExecuteAudioRendererRendering()`.
pub(crate) const IMPL_EXEC: &str = "_ZN2nn5audio6server24AudioRendererManagerImpl17AudioRendererImpl29ExecuteAudioRendererRenderingEv";

/// Follows a PLT stub (`adrp x16, page`, `ldr x17, [x16, #off]`) to the function its GOT slot holds. Anything else is
/// returned as it is.
fn plt_target(pc: usize) -> usize {
    let (Some(a), Some(l)) = (read_insn(pc), read_insn(pc + 4)) else {
        return pc;
    };
    if a & 0x9f00_001f != 0x9000_0010 || l & 0xffc0_03ff != 0xf940_0211 {
        return pc;
    }
    let imm21 = (((a >> 5) & 0x7ffff) << 2) | ((a >> 29) & 3);
    let imm = (i64::from(imm21) << 43) >> 31;
    let page = (pc as i64 & !0xfff) + imm;
    let slot = page + i64::from((l >> 10) & 0xfff) * 8;
    read_ptr(slot as usize).unwrap_or(0)
}

/// Where `name` locks its mutex: the offset in the system object, and whether it is an `nn::os::MutexType`.
fn lock_in(name: &str) -> Option<(usize, bool)> {
    let f = sym_opt(name);
    if f == 0 {
        return None;
    }
    let lock_mutex = sym_opt(LOCK_MUTEX);
    let sdk_lock = sym_opt(SDK_LOCK);
    // Bit r is set when register r holds `this` (x0 at the entry).
    let mut this: u32 = 1;
    let mut add = [-1i64; 32];
    let mut x0 = -1i64;
    for i in 0..24usize {
        let w = read_insn(f + 4 * i)?;
        let rd = (w & 31) as usize;
        let rn = ((w >> 5) & 31) as usize;
        let rm = ((w >> 16) & 31) as usize;
        if w & 0xffe0_ffe0 == 0xaa00_03e0 {
            // mov xd, xm
            if (this >> rm) & 1 != 0 {
                this |= 1 << rd;
            }
            if rd == 0 {
                x0 = add[rm];
            } else {
                add[rd] = add[rm];
            }
        } else if w & 0xffc0_0000 == 0x9100_0000 && rn != 31 && (this >> rn) & 1 != 0 {
            // add xd, x<this>, #imm12
            add[rd] = i64::from((w >> 10) & 0xfff);
            if rd == 0 {
                x0 = add[rd];
            }
        } else if w & 0xfc00_0000 == 0x9400_0000 {
            // the first bl
            let rel = i64::from(((w << 6) as i32) >> 6);
            let target = plt_target((f as i64 + 4 * (i as i64 + rel)) as usize);
            if x0 < 0 || target == 0 || (target != lock_mutex && target != sdk_lock) {
                return None;
            }
            return Some((x0 as usize, target == lock_mutex));
        }
    }
    None
}

/// The update lock, read off `AudioRenderSystem::Update` and then `UpdateImpl`. `None` leaves the caller's guess, which
/// may exclude nothing. That is logged as a severe problem.
pub(crate) fn update_lock(guess: usize) -> Option<(usize, bool)> {
    let found = lock_in(UPDATE).or_else(|| lock_in(UPDATE_IMPL));
    if found.is_none() {
        eprintln!(
            "nxkit-audio: [RED] audren: no update lock read off AudioRenderSystem::Update or UpdateImpl, keeping +{guess:#x}, where command generation may race the game's RequestUpdate"
        );
    }
    found
}

/// Where the `AudioRenderSystem` sits inside the renderer object, read off the SDK's own
/// `ExecuteAudioRendererRendering`. Its first instruction adds the offset to `this`. Anything not found gives 0x60.
pub(crate) fn impl_sys_off() -> usize {
    static FOUND: AtomicUsize = AtomicUsize::new(0);
    let k = FOUND.load(Ordering::Relaxed);
    if k != 0 {
        return k - 1;
    }
    let mut off = 0x60usize;
    let f = sym_opt(IMPL_EXEC);
    if f != 0 {
        for i in 0..16usize {
            let Some(w) = read_insn(f + 4 * i) else { break };
            if w & 0xffc0_03e0 == 0x9100_0000 {
                off = ((w >> 10) & 0xfff) as usize;
                break;
            }
            if w & 0x7c00_0000 == 0x1400_0000 {
                break;
            }
        }
    }
    if FOUND.swap(off + 1, Ordering::Relaxed) == 0 {
        eprintln!("nxkit-audio: audren: AudioRenderSystem at +{off:#x} in AudioRendererImpl");
    }
    off
}
