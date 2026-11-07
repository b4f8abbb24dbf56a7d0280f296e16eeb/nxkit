//! The memory the kernel models: the reservation's layout, the block map, thread-local areas and shared-memory backing.
//!
//! The host makes one reservation of guest memory and gives the kernel its base and size (R18). The layout inside it,
//! from the low address up, is the front region (the modules, the thread-local area, the shared-memory backing and
//! whatever else the loader puts there), the heap region, the stack region and the alias region. The heap region takes
//! what the other three leave. The kernel does not map the front region. It only records what the host tells it.

use crate::memory::{HostSpace, PAGE_SIZE};
use crate::space::{MemoryMap, perm, state};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// The size of the thread-local slot of one thread: 8 KiB TLS block, then the thread-local region of 0x200 bytes,
/// then the 0x400 bytes of the thread object, rounded up to 16 KiB so that a slot is a whole host page.
pub(crate) const TLS_SLOT: u64 = 0x4000;
/// The TLS block (`tpidr_el0`) in a slot.
pub(crate) const TLS_OFFSET: u64 = 0;
/// The thread-local region (`tpidrro_el0`) in a slot. Its first 0x100 bytes are the IPC buffer.
pub(crate) const TLR_OFFSET: u64 = 0x2000;
/// The thread object, behind the thread-local region.
pub(crate) const THREAD_OBJECT_OFFSET: u64 = TLR_OFFSET + 0x200;
/// The size of the IPC buffer in the thread-local region.
pub(crate) const IPC_BUFFER_SIZE: u64 = 0x100;
/// Where the thread-local region holds the id of its thread (a 4-byte word the SDK reads).
pub(crate) const TLR_THREAD_ID: u64 = 0x110;
/// Where the thread-local region holds the address of the thread object.
pub(crate) const TLR_THREAD_OBJECT: u64 = 0x1F8;

/// One module in the front region. Its segments follow each other: text, rodata, data, then bss. Sizes round up to
/// whole 4 KiB pages.
#[derive(Clone, Copy, Debug)]
pub struct Module {
    /// The address of the text segment.
    pub base: u64,
    /// The size of the text segment, which is executable.
    pub text: u64,
    /// The size of the read-only data segment.
    pub rodata: u64,
    /// The size of the data segment.
    pub data: u64,
    /// The size of the bss segment.
    pub bss: u64,
}

/// What the host tells the kernel about the guest's memory. The host reads most values from `game.json`.
#[derive(Clone)]
pub struct MemoryConfig {
    /// Makes the host's pages match the kernel's model.
    pub host: Arc<dyn HostSpace>,
    /// The base address of the reservation.
    pub base: u64,
    /// The size of the reservation.
    pub size: u64,
    /// The size of the front region at the start of the reservation. The heap region begins behind it.
    pub front_size: u64,
    /// The size of the stack region.
    pub stack_size: u64,
    /// The size of the alias region, at the end of the reservation.
    pub alias_size: u64,
    /// The memory pool of the application in bytes (total memory in `GetInfo`).
    pub app_pool: u64,
    /// What the process uses besides its heap, in bytes. It counts as used memory in `GetInfo` and against the pool.
    pub used_memory: u64,
    /// The system resource size of the NPDM. Zero means the title has none.
    pub system_resource: u64,
    /// Whether pages mapped by `MapPhysicalMemory` count against the pool.
    pub map_limit: bool,
    /// The program id that `GetInfo` returns.
    pub program_id: u64,
    /// The area for thread-local slots: `(address, size)`, inside the front region and readable and writable.
    /// The first 0x4000 bytes are the user exception context. Each thread takes 0x4000 bytes of the rest.
    pub tls: (u64, u64),
    /// The area that backs shared memory: `(address, size)`, inside the front region and readable and writable.
    /// Memory is taken from it in whole host pages and is not given back.
    pub shared: (u64, u64),
    /// The stack of the main thread: `(address, size)`, inside the stack region, or none.
    pub main_stack: Option<(u64, u64)>,
    /// The modules in the front region.
    pub modules: Vec<Module>,
}

/// The regions of the reservation.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    pub base: u64,
    pub end: u64,
    pub heap_base: u64,
    pub heap_cap: u64,
    pub stack_base: u64,
    pub stack_end: u64,
    pub alias_base: u64,
    pub alias_end: u64,
}

/// The map and the counters that change with it. One lock covers them, held for one update and never across a wait.
pub(crate) struct Space {
    pub map: MemoryMap,
    pub heap_size: u64,
    pub physical_used: u64,
}

/// The kernel's memory.
pub(crate) struct Vm {
    pub cfg: MemoryConfig,
    pub layout: Layout,
    pub space: Mutex<Space>,
    tls_free: Mutex<Vec<u64>>,
    shared_next: AtomicU64,
}

const fn align_up(v: u64, to: u64) -> u64 {
    v.div_ceil(to) * to
}

impl Vm {
    /// Checks the layout and builds the model. The error names the first value that does not fit.
    pub(crate) fn new(cfg: MemoryConfig) -> Result<Vm, String> {
        let page = cfg.host.page_size().max(PAGE_SIZE);
        let aligned = |name: &str, v: u64| {
            if v.is_multiple_of(page) {
                Ok(())
            } else {
                Err(format!("{name} {v:#x} is not a multiple of {page:#x}"))
            }
        };
        aligned("base", cfg.base)?;
        aligned("size", cfg.size)?;
        aligned("front_size", cfg.front_size)?;
        aligned("stack_size", cfg.stack_size)?;
        aligned("alias_size", cfg.alias_size)?;
        let fixed = cfg
            .front_size
            .checked_add(cfg.stack_size)
            .and_then(|v| v.checked_add(cfg.alias_size))
            .filter(|v| *v < cfg.size)
            .ok_or("front, stack and alias regions leave no room for the heap region")?;
        let end = cfg
            .base
            .checked_add(cfg.size)
            .filter(|e| *e <= crate::space::SPACE_END)
            .ok_or("the reservation is not inside the 39-bit address space")?;
        let layout = Layout {
            base: cfg.base,
            end,
            heap_base: cfg.base + cfg.front_size,
            heap_cap: cfg.size - fixed,
            stack_base: cfg.base + cfg.size - cfg.alias_size - cfg.stack_size,
            stack_end: end - cfg.alias_size,
            alias_base: end - cfg.alias_size,
            alias_end: end,
        };
        let front = cfg.base..layout.heap_base;
        let inside = |name: &str, (addr, size): (u64, u64)| {
            if size > 0 && front.contains(&addr) && addr + size <= front.end {
                Ok(())
            } else {
                Err(format!(
                    "{name} {addr:#x}+{size:#x} is not inside the front region"
                ))
            }
        };
        inside("tls area", cfg.tls)?;
        inside("shared area", cfg.shared)?;
        aligned("tls area", cfg.tls.0)?;
        aligned("shared area", cfg.shared.0)?;
        if !cfg.tls.0.is_multiple_of(TLS_SLOT) || cfg.tls.1 < 2 * TLS_SLOT {
            return Err(format!(
                "the tls area needs {TLS_SLOT:#x} alignment and room for the exception context and one thread"
            ));
        }
        if let Some((addr, size)) = cfg.main_stack
            && !(addr >= layout.stack_base && addr + size <= layout.stack_end && size > 0)
        {
            return Err(format!(
                "main stack {addr:#x}+{size:#x} is not inside the stack region"
            ));
        }

        let mut map = MemoryMap::new();
        for m in &cfg.modules {
            let text = m.base + align_up(m.text, PAGE_SIZE);
            let ro = text + align_up(m.rodata, PAGE_SIZE);
            let data = ro + align_up(m.data, PAGE_SIZE);
            let bss = data + align_up(m.bss, PAGE_SIZE);
            if !m.base.is_multiple_of(PAGE_SIZE) || !front.contains(&m.base) || bss > front.end {
                return Err(format!(
                    "module {:#x} is not inside the front region",
                    m.base
                ));
            }
            map.set(m.base, text, state::CODE, perm::RX, 0, 0);
            map.set(text, ro, state::CODE, perm::R, 0, 0);
            map.set(ro, bss, state::CODE_DATA, perm::RW, 0, 0);
        }
        map.set(
            cfg.tls.0,
            cfg.tls.0 + cfg.tls.1,
            state::THREAD_LOCAL,
            perm::RW,
            0,
            0,
        );
        map.set(
            layout.heap_base,
            layout.heap_base + layout.heap_cap,
            state::FREE,
            perm::NONE,
            0,
            0,
        );
        map.set(
            layout.stack_base,
            layout.alias_end,
            state::FREE,
            perm::NONE,
            0,
            0,
        );
        if let Some((addr, size)) = cfg.main_stack {
            map.set(addr, addr + size, state::STACK, perm::RW, 0, 0);
        }
        let mut slots: Vec<u64> = (cfg.tls.0 + TLS_SLOT..cfg.tls.0 + cfg.tls.1 - TLS_SLOT + 1)
            .step_by(TLS_SLOT as usize)
            .collect();
        slots.reverse();
        Ok(Vm {
            cfg,
            layout,
            space: Mutex::new(Space {
                map,
                heap_size: 0,
                physical_used: 0,
            }),
            tls_free: Mutex::new(slots),
            shared_next: AtomicU64::new(0),
        })
    }

    /// The address of the user exception context, a 0x1000-byte area that is never given to a thread.
    pub(crate) fn exception_context(&self) -> u64 {
        self.cfg.tls.0
    }

    pub(crate) fn tls_acquire(&self) -> Option<u64> {
        crate::thread::lock(&self.tls_free).pop()
    }

    pub(crate) fn tls_release(&self, slot: u64) {
        crate::thread::lock(&self.tls_free).push(slot);
    }

    /// Takes `size` bytes of the shared-memory backing, rounded up to whole host pages. `None` when it is used up.
    pub(crate) fn shared_alloc(&self, size: u64) -> Option<(u64, u64)> {
        let page = self.cfg.host.page_size().max(PAGE_SIZE);
        let size = align_up(size, page);
        let (base, total) = self.cfg.shared;
        let mut used = self.shared_next.load(Ordering::Relaxed);
        loop {
            let next = used.checked_add(size).filter(|n| *n <= total)?;
            match self
                .shared_next
                .compare_exchange(used, next, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => return Some((base + used, size)),
                Err(now) => used = now,
            }
        }
    }
}
