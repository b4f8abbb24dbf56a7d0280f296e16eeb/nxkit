//! The environment of a guest thread (R46).
//!
//! The rewritten code reaches its thread state through four thread-specific slots (the key page of each module,
//! see `docs/nxkit-host.md`). `K18` holds what the guest's `x18` held. `KTLS` is the ELF thread-local block
//! (`tpidr_el0`). `KTLR` is the Horizon thread-local region (`tpidrro_el0`), which the kernel and the SDK read.
//! `KGATE` is the address of the gate stub of the thread.
//!
//! Every thread owns a slot `k`. Slot `k` is a gate stub and a gate stack. The thread-local region and the TLS block
//! come from the kernel (`nxkit_kernel_thread_tls`), which keeps them inside the reservation because it reads the IPC
//! message from the region.

use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::boot::Boot;
use crate::error::{HostError, Result};
use crate::gate::{self, GATE_SLOTS, GATE_STACK_SIZE};
use crate::layout::PAGE;
use crate::{seams, stop, sys};

/// The four pthread keys. The index of a key is its place in the key page.
#[derive(Debug, Clone, Copy)]
pub struct Keys {
    /// The value of the guest's `x18`.
    pub k18: usize,
    /// The ELF TLS block.
    pub ktls: usize,
    /// The thread-local region.
    pub ktlr: usize,
    /// The gate stub address.
    pub kgate: usize,
}

impl Keys {
    /// The keys in the order of the key page.
    pub fn in_page_order(&self) -> [usize; 4] {
        [self.k18, self.ktls, self.ktlr, self.kgate]
    }
}

static KEYS: OnceLock<Keys> = OnceLock::new();

/// Creates the four keys. It runs once, before the key pages are filled.
pub fn create_keys() -> Result<Keys> {
    let keys = Keys {
        k18: sys::key_create()?,
        ktls: sys::key_create()?,
        ktlr: sys::key_create()?,
        kgate: sys::key_create()?,
    };
    KEYS.set(keys)
        .map_err(|_| HostError::Load("the thread keys are created twice".into()))?;
    Ok(keys)
}

fn keys() -> Result<Keys> {
    KEYS.get()
        .copied()
        .ok_or_else(|| HostError::Load("the thread keys are not created yet".into()))
}

/// One bit per gate slot. A set bit means that a guest thread uses the slot. Taking and giving a slot is lock free (R7).
static USED: [AtomicU64; GATE_SLOTS / 64] = [const { AtomicU64::new(0) }; GATE_SLOTS / 64];

fn take_slot() -> Result<usize> {
    for (w, word) in USED.iter().enumerate() {
        let mut cur = word.load(Ordering::Relaxed);
        while cur != u64::MAX {
            let bit = (!cur).trailing_zeros();
            match word.compare_exchange_weak(
                cur,
                cur | (1 << bit),
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(w * 64 + bit as usize),
                Err(now) => cur = now,
            }
        }
    }
    Err(HostError::Memory(format!(
        "more than {GATE_SLOTS} guest threads at the same time"
    )))
}

fn give_slot(k: usize) {
    if let Some(word) = USED.get(k / 64) {
        word.fetch_and(!(1 << (k % 64)), Ordering::AcqRel);
    }
}

/// The state of one guest thread. It belongs to the host thread that created it.
#[derive(Debug)]
pub struct ThreadEnv {
    slot: usize,
    tlr: u64,
    gate_stack: u64,
    gate_stack_len: u64,
}

impl ThreadEnv {
    /// Sets up the slots of the calling host thread.
    pub fn new() -> Result<ThreadEnv> {
        let k = keys()?;
        let slot = take_slot()?;
        let len = GATE_STACK_SIZE + PAGE;
        let gate_stack = match sys::map_stack(len, PAGE) {
            Ok(b) => b,
            Err(e) => {
                give_slot(slot);
                return Err(e);
            }
        };
        let mut env = ThreadEnv {
            slot,
            tlr: 0,
            gate_stack,
            gate_stack_len: len,
        };
        match env.fill(k) {
            Ok(()) => Ok(env),
            Err(e) => {
                env.release();
                Err(e)
            }
        }
    }

    fn fill(&mut self, k: Keys) -> Result<()> {
        let (tlr, tls) = sys::kernel_thread_tls()?;
        self.tlr = tlr;
        gate::set_slot_top(self.slot, self.gate_stack + self.gate_stack_len);
        sys::key_set(k.k18, 0)?;
        sys::key_set(k.ktls, tls)?;
        sys::key_set(k.ktlr, tlr)?;
        sys::key_set(k.kgate, gate::stub_address(self.slot))
    }

    /// The thread-local region of the thread.
    pub fn tlr(&self) -> u64 {
        self.tlr
    }

    /// Gives the slot and the gate stack back. The thread must not run guest code after this.
    pub fn release(self) {
        sys::free_range(self.gate_stack, self.gate_stack_len);
        give_slot(self.slot);
    }
}

fn run(entry: u64, arg: u64, arg1: u64, sp: u64) -> Result<()> {
    let env = ThreadEnv::new()?;
    gate::enter(entry, arg, arg1, sp);
    env.release();
    Ok(())
}

/// The function the kernel calls on the host thread of every guest thread that the game creates.
pub extern "C" fn guest_thread_entry(entry: u64, arg: u64, stack_top: u64, _user: *mut c_void) {
    if let Err(e) = run(entry, arg, 0, stack_top) {
        stop::fatal(&format!("cannot start a guest thread: {e}"));
    }
}

/// Starts the main guest thread on a host thread of its own. The thread attaches to the kernel with the priority and
/// the core of the NPDM, which the kernel got at its start. Then it runs the boot of the SDK and `nnMain` on `stack_top`
/// (R57). The handle that the attach returns is the `x0` of `nnosInitialize`.
pub fn start_main(boot: Boot, stack_top: u64) -> Result<()> {
    std::thread::Builder::new()
        .name("guest-main".into())
        .spawn(move || {
            let handle = match seams::attach_main_thread() {
                Ok(h) => h,
                Err(e) => stop::fatal(&format!("cannot attach the main guest thread: {e}")),
            };
            match ThreadEnv::new() {
                Ok(env) => {
                    boot.run(handle, stack_top);
                    env.release();
                }
                Err(e) => stop::fatal(&format!("cannot run the main guest thread: {e}")),
            }
            stop::request("the main guest thread ended");
        })
        .map(|_| ())
        .map_err(|e| HostError::App(format!("cannot start the main guest thread: {e}")))
}
