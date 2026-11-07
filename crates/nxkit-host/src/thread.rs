//! The environment of a guest thread (R46).
//!
//! The rewritten code reaches its thread state through four thread-specific slots (the key page of each module,
//! see `docs/nxkit-host.md`). `K18` holds what the guest's `x18` held. `KTLS` is the ELF thread-local block
//! (`tpidr_el0`). `KTLR` is the Horizon thread-local region (`tpidrro_el0`), which the kernel and the SDK read.
//! `KGATE` is the address of the gate stub of the thread.
//!
//! Every thread owns a slot `k`. Slot `k` is a gate stub, a gate stack and a 16 KiB block inside the reservation.
//! The block holds the thread-local region (0x200 bytes), the thread object (0x400 bytes) and the TLS (8 KiB). The
//! block lies inside the reservation, because the kernel reads the IPC message from the region.

use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use crate::error::{HostError, Result};
use crate::gate::{self, GATE_SLOTS, GATE_STACK_SIZE};
use crate::layout::{PAGE, TLS_SLOT_SIZE};
use crate::manifest::MainThread;
use crate::{memory, seams, stop, sys};

/// The offset of the thread object in a block.
const OBJECT_OFFSET: u64 = 0x200;
/// The offset of the ELF TLS in a block.
const TLS_OFFSET: u64 = 0x600;
/// The size of the ELF TLS. The SDK's own TLS image is far smaller.
const TLS_SIZE: u64 = 8192;
/// The offset in the thread-local region of the pointer to the current thread object.
const TLR_THREAD_OBJECT: u64 = 0x1f8;
/// The offset in the thread-local region of the thread ID. The SDK reads it since version 22.
const TLR_THREAD_ID: u64 = 0x110;
/// The pseudo handle of the current thread.
const CURRENT_THREAD: u64 = 0xFFFF_8000;
/// The svc `GetThreadId`.
const SVC_GET_THREAD_ID: u32 = 0x25;

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

fn free_slots() -> &'static Mutex<Vec<usize>> {
    static FREE: OnceLock<Mutex<Vec<usize>>> = OnceLock::new();
    FREE.get_or_init(|| Mutex::new((0..GATE_SLOTS).rev().collect()))
}

fn take_slot() -> Result<usize> {
    let mut g = free_slots().lock().unwrap_or_else(|e| e.into_inner());
    g.pop()
        .ok_or_else(|| HostError::Memory(format!("more than {GATE_SLOTS} guest threads at the same time")))
}

fn give_slot(k: usize) {
    free_slots().lock().unwrap_or_else(|e| e.into_inner()).push(k);
}

/// The state of one guest thread. It belongs to the host thread that created it.
#[derive(Debug)]
pub struct ThreadEnv {
    slot: usize,
    block: u64,
    gate_stack: u64,
    gate_stack_len: u64,
}

impl ThreadEnv {
    /// Sets up the slots of the calling host thread.
    pub fn new() -> Result<ThreadEnv> {
        let guest = memory::get()?;
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
        let env = ThreadEnv {
            slot,
            block: guest.layout.tls.start + slot as u64 * TLS_SLOT_SIZE,
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

    fn fill(&self, k: Keys) -> Result<()> {
        let guest = memory::get()?;
        guest.res.zero(self.block, TLS_SLOT_SIZE)?;
        guest.res.write_u64(self.block + TLR_THREAD_OBJECT, self.block + OBJECT_OFFSET)?;
        gate::set_slot_top(self.slot, self.gate_stack + self.gate_stack_len);
        sys::key_set(k.k18, 0)?;
        sys::key_set(k.ktls, self.block + TLS_OFFSET)?;
        sys::key_set(k.ktlr, self.block)?;
        sys::key_set(k.kgate, gate::stub_address(self.slot))
    }

    /// The thread-local region of the thread.
    pub fn tlr(&self) -> u64 {
        self.block
    }

    /// Writes the kernel's ID of the thread into the thread-local region.
    pub fn set_thread_id(&self, id: u32) -> Result<()> {
        memory::get()?
            .res
            .copy_in(self.block + TLR_THREAD_ID, &id.to_le_bytes())
    }

    /// Gives the slot and the gate stack back. The thread must not run guest code after this.
    pub fn release(self) {
        sys::free_range(self.gate_stack, self.gate_stack_len);
        give_slot(self.slot);
    }
}

/// The ID that the kernel gave the calling thread, from `GetThreadId` of the current-thread handle.
fn kernel_thread_id() -> Result<u32> {
    let mut regs = [0u64; 8];
    regs[1] = CURRENT_THREAD;
    if gate::call_svc(SVC_GET_THREAD_ID, &mut regs) == 0 {
        return Err(HostError::Unwired("svc GetThreadId (0x25) in the kernel"));
    }
    if regs[0] != 0 {
        return Err(HostError::App(format!("GetThreadId failed with {:#x}", regs[0])));
    }
    u32::try_from(regs[1]).map_err(|_| HostError::App("GetThreadId returned an ID above 32 bits".into()))
}

fn run(entry: u64, arg: u64, arg1: u64, sp: u64) -> Result<()> {
    let env = ThreadEnv::new()?;
    let id = kernel_thread_id();
    match id {
        Ok(id) => env.set_thread_id(id)?,
        Err(e) => {
            env.release();
            return Err(e);
        }
    }
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
/// the core of the NPDM, then runs `entry` on `stack_top`.
pub fn start_main(entry: u64, stack_top: u64, spec: MainThread) -> Result<()> {
    std::thread::Builder::new()
        .name("guest-main".into())
        .spawn(move || {
            if let Err(e) = seams::attach_main_thread(&spec) {
                stop::fatal(&format!("cannot attach the main guest thread: {e}"));
            }
            if let Err(e) = run(entry, 0, CURRENT_THREAD, stack_top) {
                stop::fatal(&format!("cannot run the main guest thread: {e}"));
            }
            stop::request("the main guest thread ended");
        })
        .map(|_| ())
        .map_err(|e| HostError::App(format!("cannot start the main guest thread: {e}")))
}
