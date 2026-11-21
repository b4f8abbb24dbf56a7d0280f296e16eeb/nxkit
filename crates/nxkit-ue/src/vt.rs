//! The vtable the guest calls, and the gate handler that installs it.

use crate::heap::Heap;
use crate::layout::{Layout, layout_for_name_slot};
use nxkit_abi::guest;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

struct State {
    heap: Heap,
    fatal: fn(&str) -> !,
}

/// The engine heap.
// Process-wide (W26): the guest calls the slots of the vtable directly, with no Rust context.
static STATE: OnceLock<State> = OnceLock::new();

fn state() -> &'static State {
    match STATE.get() {
        Some(s) => s,
        // The vtable exists only after `install` set the state, so a call without it cannot happen.
        None => std::process::abort(),
    }
}

fn gib(v: u64) -> f64 {
    v as f64 / f64::from(1u32 << 30)
}

fn out_of_memory(s: &State, what: &str, size: u64, align: u32) -> ! {
    let (used, _) = s.heap.in_use();
    (s.fatal)(&format!(
        "Unreal heap: out of memory in {what} ({size} bytes, alignment {align}, {:.2} GiB in use of {:.2} GiB); \
         NXKIT_UE_HEAP sets the arena",
        gib(used),
        gib(s.heap.range().1 - s.heap.range().0)
    ))
}

fn resize(s: &State, p: u64, size: u64, align: u32) -> Option<u64> {
    if p == 0 {
        return s.heap.alloc(size, u64::from(align));
    }
    if size == 0 {
        s.heap.free(p);
        return None;
    }
    let Some(old) = s.heap.size_of(p) else {
        (s.fatal)(&format!(
            "Unreal heap: Realloc of {p:#x}, which is not a block of the heap"
        ))
    };
    if Heap::keeps(p, old, size, u64::from(align)) {
        return Some(p);
    }
    let q = s.heap.alloc(size, u64::from(align))?;
    let n = old.min(size);
    if guest::with_slice(p, n, |src| guest::write(q, src)).is_err() {
        (s.fatal)(&format!(
            "Unreal heap: the copy of {p:#x} to {q:#x} left the guest range"
        ));
    }
    s.heap.free(p);
    Some(q)
}

extern "C" fn m_malloc(_this: u64, size: u64, align: u32) -> u64 {
    let s = state();
    s.heap
        .alloc(size, u64::from(align))
        .unwrap_or_else(|| out_of_memory(s, "Malloc", size, align))
}

extern "C" fn m_try_malloc(_this: u64, size: u64, align: u32) -> u64 {
    state().heap.alloc(size, u64::from(align)).unwrap_or(0)
}

extern "C" fn m_realloc(_this: u64, p: u64, size: u64, align: u32) -> u64 {
    let s = state();
    match resize(s, p, size, align) {
        Some(q) => q,
        None if size == 0 => 0,
        None => out_of_memory(s, "Realloc", size, align),
    }
}

extern "C" fn m_try_realloc(_this: u64, p: u64, size: u64, align: u32) -> u64 {
    resize(state(), p, size, align).unwrap_or(0)
}

extern "C" fn m_free(_this: u64, p: u64) {
    if p != 0 {
        state().heap.free(p);
    }
}

extern "C" fn m_quantize(_this: u64, size: u64, align: u32) -> u64 {
    state().heap.quantize(size, u64::from(align))
}

extern "C" fn m_get_size(_this: u64, p: u64, out: u64) -> bool {
    match state().heap.size_of(p) {
        Some(n) => guest::write_u64(out, n).is_ok(),
        None => false,
    }
}

extern "C" fn m_nothing(_this: u64) {}

/// The host's functions in the slots of `layout`, over the guest's vtable `guest_vt` (the other slots stay).
pub(crate) fn fill(layout: &Layout, guest_vt: &[u64]) -> Vec<u64> {
    let mut v = guest_vt.to_vec();
    v.resize(layout.slots, 0);
    let f3 = |f: extern "C" fn(u64, u64, u32) -> u64| f as usize as u64;
    let f4 = |f: extern "C" fn(u64, u64, u64, u32) -> u64| f as usize as u64;
    v[layout.malloc] = f3(m_malloc);
    v[layout.realloc] = f4(m_realloc);
    if layout.try_malloc != 0 {
        v[layout.try_malloc] = f3(m_try_malloc);
    }
    if layout.try_realloc != 0 {
        v[layout.try_realloc] = f4(m_try_realloc);
    }
    v[layout.free] = m_free as extern "C" fn(u64, u64) as usize as u64;
    v[layout.quantize] = f3(m_quantize);
    v[layout.get_size] = m_get_size as extern "C" fn(u64, u64, u64) -> bool as usize as u64;
    for &s in layout.nothing {
        v[s] = m_nothing as extern "C" fn(u64) as usize as u64;
    }
    v
}

/// The installed heap: the handler of the allocator gate.
pub struct Installed {
    vtable: u64,
    layout: &'static Layout,
    logged: AtomicBool,
}

impl Installed {
    /// The guest address of the host's vtable.
    pub fn vtable(&self) -> u64 {
        self.vtable
    }

    /// The layout of the vtable.
    pub fn layout(&self) -> &'static Layout {
        self.layout
    }

    /// The handler of the gate on the cut constructor call: the object is in `regs[param]`. It gets the host's
    /// vtable before anything calls it.
    pub fn on_object(&self, regs: &mut [u64], param: u32) {
        let Some(&obj) = regs.get(param as usize) else {
            return;
        };
        if guest::write_u64(obj, self.vtable).is_err() {
            eprintln!(
                "nxkit-ue: [RED] Unreal's allocator object {obj:#x} is outside the guest range: it has no vtable"
            );
            return;
        }
        if !self.logged.swap(true, Ordering::Relaxed) {
            let (a, b) = state().heap.range();
            eprintln!(
                "nxkit-ue: Unreal's allocator (FMallocBinned3 at {obj:#x}, {}) runs on the host heap, \
                 {:.2} GiB of the guest block at {a:#x}..{b:#x}",
                self.layout.name,
                gib(b - a)
            );
        }
    }
}

/// Makes the engine heap over the arena `(address, size)` and its vtable from the guest's vtable at `guest_vt`
/// (relocated) with the name getter at `name_slot`. `fatal` ends the run when the heap is used up. It runs once.
pub fn install(
    guest_vt: u64,
    name_slot: u32,
    arena: (u64, u64),
    fatal: fn(&str) -> !,
) -> Result<Installed, String> {
    let layout = layout_for_name_slot(name_slot).ok_or_else(|| {
        format!("Unreal's FMallocBinned3 has its name getter at slot {name_slot}, a layout nxkit-ue does not know")
    })?;
    let mut words = vec![0u64; layout.slots];
    for (i, w) in words.iter_mut().enumerate() {
        *w = guest::read_u64(guest_vt + 8 * i as u64)
            .map_err(|_| format!("the vtable at {guest_vt:#x} is outside the guest range"))?;
    }
    let words = fill(layout, &words);
    let heap = Heap::new(arena.0, arena.1, |l| eprintln!("nxkit-ue: {l}"));
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let vtable = heap
        .alloc(bytes.len() as u64, 16)
        .ok_or("the arena has no room for the vtable")?;
    guest::write(vtable, &bytes).map_err(|_| "the arena is outside the guest range".to_string())?;
    STATE
        .set(State { heap, fatal })
        .map_err(|_| "the Unreal heap is installed twice".to_string())?;
    Ok(Installed {
        vtable,
        layout,
        logged: AtomicBool::new(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::LAYOUTS;

    #[test]
    fn the_host_fills_its_slots_and_keeps_the_guests_other_slots() {
        let l = &LAYOUTS[1];
        let guest: Vec<u64> = (0..l.slots as u64).map(|i| 0x1000 + i).collect();
        let v = fill(l, &guest);
        assert_eq!(v.len(), l.slots);
        assert_eq!(
            v[l.malloc],
            m_malloc as extern "C" fn(u64, u64, u32) -> u64 as usize as u64
        );
        assert_eq!(v[l.free], m_free as extern "C" fn(u64, u64) as usize as u64);
        for s in [0, 1, 2, l.name_slot as usize, 13, 14, 15, 17, 18, 20] {
            assert_eq!(v[s], guest[s], "slot {s} keeps the guest's function");
        }
    }
}
