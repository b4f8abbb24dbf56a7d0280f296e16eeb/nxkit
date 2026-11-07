//! Every call into the system that needs `unsafe`: mmap, dyld, pthread keys, Mach and the window (R45).
//!
//! The rest of the host sees safe types. A [`Reservation`] checks every address against its own range before it
//! touches memory.

pub mod appkit;

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

use crate::error::{HostError, Result};
use crate::macho;

/// `PROT_NONE`.
pub const PROT_NONE: i32 = 0;
/// `PROT_READ`.
pub const PROT_R: i32 = 1;
/// `PROT_READ | PROT_WRITE`.
pub const PROT_RW: i32 = 3;
/// `PROT_READ | PROT_EXEC`.
pub const PROT_RX: i32 = 5;

const MAP_PRIVATE: c_int = 0x2;
const MAP_FIXED: c_int = 0x10;
const MAP_NORESERVE: c_int = 0x40;
const MAP_ANON: c_int = 0x1000;
const RTLD_NOW: c_int = 2;
const RTLD_LOCAL: c_int = 4;
const VM_FLAGS_FIXED: c_int = 0;

unsafe extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, off: i64) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
    fn mprotect(addr: *mut c_void, len: usize, prot: c_int) -> c_int;
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlerror() -> *const c_char;
    fn _dyld_image_count() -> u32;
    fn _dyld_get_image_name(index: u32) -> *const c_char;
    fn _dyld_get_image_header(index: u32) -> *const c_void;
    fn _dyld_get_image_vmaddr_slide(index: u32) -> isize;
    fn pthread_key_create(key: *mut usize, destructor: Option<extern "C" fn(*mut c_void)>) -> c_int;
    fn pthread_setspecific(key: usize, value: *const c_void) -> c_int;
    fn pthread_getspecific(key: usize) -> *mut c_void;
    fn mach_vm_allocate(task: u32, addr: *mut u64, size: u64, flags: c_int) -> c_int;
    fn mach_vm_deallocate(task: u32, addr: u64, size: u64) -> c_int;
    static mach_task_self_: u32;
}

fn map_failed() -> *mut c_void {
    usize::MAX as *mut c_void
}

fn mem_err(what: &str, errno: i32) -> HostError {
    HostError::Memory(format!("{what} failed, errno {errno}"))
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// The one range of address space that holds all guest memory. It is never unmapped.
#[derive(Debug)]
pub struct Reservation {
    base: u64,
    len: u64,
}

impl Reservation {
    /// The first address.
    pub fn base(&self) -> u64 {
        self.base
    }

    /// The length in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// True when the range is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn check(&self, addr: u64, len: u64) -> Result<*mut c_void> {
        let end = addr.checked_add(len);
        match end {
            Some(e) if addr >= self.base && e <= self.base + self.len => Ok(addr as usize as *mut c_void),
            _ => Err(HostError::Memory(format!(
                "range {addr:#x} + {len:#x} is outside the reservation {:#x} + {:#x}",
                self.base, self.len
            ))),
        }
    }

    /// Changes the protection of pages inside the reservation.
    pub fn protect(&self, addr: u64, len: u64, prot: i32) -> Result<()> {
        let p = self.check(addr, len)?;
        // SAFETY: the range lies inside the reservation, which this process owns.
        if unsafe { mprotect(p, len as usize, prot) } != 0 {
            return Err(mem_err("mprotect", errno()));
        }
        Ok(())
    }

    /// Gives a range back to the system, so that dyld can use it. The range stays a hole until `map_anon` fills it.
    pub fn unmap(&self, addr: u64, len: u64) -> Result<()> {
        let p = self.check(addr, len)?;
        // SAFETY: the range lies inside the reservation. The caller does not use it until it maps it again.
        if unsafe { munmap(p, len as usize) } != 0 {
            return Err(mem_err("munmap", errno()));
        }
        Ok(())
    }

    /// Maps anonymous memory into a hole. The call has no `MAP_FIXED`, so it cannot replace a mapping that
    /// something else owns. It fails if the system chose another address.
    pub fn map_anon(&self, addr: u64, len: u64, prot: i32) -> Result<()> {
        let p = self.check(addr, len)?;
        // SAFETY: without `MAP_FIXED` the system never replaces an existing mapping.
        let got = unsafe { mmap(p, len as usize, prot, MAP_PRIVATE | MAP_ANON, -1, 0) };
        if got == map_failed() {
            return Err(mem_err("mmap", errno()));
        }
        if got != p {
            // SAFETY: `got` was just mapped here with `len` bytes.
            unsafe { munmap(got, len as usize) };
            return Err(HostError::Memory(format!(
                "{addr:#x} is taken, the system gave {:#x}",
                got as usize
            )));
        }
        Ok(())
    }

    /// Copies bytes in. The pages must be writable.
    pub fn copy_in(&self, addr: u64, bytes: &[u8]) -> Result<()> {
        let p = self.check(addr, bytes.len() as u64)?;
        // SAFETY: the range lies inside the reservation. The pages are writable by the contract of this function.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), bytes.len()) };
        Ok(())
    }

    /// Writes zeros. The pages must be writable.
    pub fn zero(&self, addr: u64, len: u64) -> Result<()> {
        let p = self.check(addr, len)?;
        // SAFETY: as for `copy_in`.
        unsafe { std::ptr::write_bytes(p.cast::<u8>(), 0, len as usize) };
        Ok(())
    }

    /// Writes a little endian 64-bit value.
    pub fn write_u64(&self, addr: u64, value: u64) -> Result<()> {
        self.copy_in(addr, &value.to_le_bytes())
    }

    /// Reads a little endian 64-bit value. The page must be readable.
    pub fn read_u64(&self, addr: u64) -> Result<u64> {
        let p = self.check(addr, 8)?;
        let mut b = [0u8; 8];
        // SAFETY: the range lies inside the reservation and the page is readable by the contract.
        unsafe { std::ptr::copy_nonoverlapping(p.cast::<u8>(), b.as_mut_ptr(), 8) };
        Ok(u64::from_le_bytes(b))
    }

    /// Wraps the whole range as the buffer of the GPU backend (R3).
    pub fn as_guest_memory(&self, ctx: &nxkit_metal::Context) -> Result<nxkit_metal::GuestMemory> {
        let base = NonNull::new(self.base as usize as *mut u8)
            .ok_or_else(|| HostError::Memory("the reservation has no address".into()))?;
        // SAFETY: the reservation is mapped for `len` bytes and is never unmapped or moved.
        unsafe { nxkit_metal::GuestMemory::from_raw(ctx, base, self.len) }
            .map_err(|e| HostError::Memory(format!("the GPU buffer over the reservation: {e}")))
    }
}

/// Reserves `[tail_start, end)` as address space with no access, behind an image that dyld mapped, and returns a view
/// over `[view_start, end)`. The part `[view_start, tail_start)` is what dyld mapped. The view lets the host protect
/// and fill those pages. The call fails when the tail is not free.
pub fn reserve_tail(view_start: u64, tail_start: u64, end: u64) -> Result<Reservation> {
    if view_start > tail_start || tail_start >= end {
        return Err(HostError::Memory(format!(
            "the tail {tail_start:#x}..{end:#x} does not follow the view start {view_start:#x}"
        )));
    }
    let len = end - tail_start;
    let got = map_hint(tail_start, len, PROT_NONE)?;
    if got != tail_start {
        // SAFETY: `got` was just mapped here with `len` bytes.
        unsafe { munmap(got as usize as *mut c_void, len as usize) };
        return Err(HostError::Memory(format!(
            "the guest range {tail_start:#x} + {len:#x} behind the library is not free, the system gave {got:#x}"
        )));
    }
    Ok(Reservation {
        base: view_start,
        len: end - view_start,
    })
}

/// Reserves `[start, start + len)` as address space with no access and no commit. The call fails when the system
/// gives another address, because the guest needs exactly this one (R18).
pub fn reserve(start: u64, len: u64) -> Result<Reservation> {
    let got = map_hint(start, len, PROT_NONE)?;
    if got != start {
        // SAFETY: `got` was just mapped here with `len` bytes.
        unsafe { munmap(got as usize as *mut c_void, len as usize) };
        return Err(HostError::Memory(format!(
            "the guest range {start:#x} + {len:#x} is not free in this process, the system gave {got:#x}"
        )));
    }
    Ok(Reservation { base: start, len })
}

/// Maps anonymous memory with `addr` as a hint and returns the address that the system chose.
pub fn map_hint(addr: u64, len: u64, prot: i32) -> Result<u64> {
    let flags = MAP_PRIVATE | MAP_ANON | MAP_NORESERVE;
    // SAFETY: without `MAP_FIXED` the system never replaces an existing mapping.
    let got = unsafe { mmap(addr as usize as *mut c_void, len as usize, prot, flags, -1, 0) };
    if got == map_failed() {
        return Err(mem_err("mmap", errno()));
    }
    Ok(got as usize as u64)
}

/// Maps anonymous memory at `addr` with `MAP_FIXED`. It replaces what is there. Only the probe uses it, on ranges that
/// it knows to be free.
pub fn map_fixed(addr: u64, len: u64) -> Result<()> {
    let flags = MAP_PRIVATE | MAP_ANON | MAP_NORESERVE | MAP_FIXED;
    // SAFETY: the probe calls this only for ranges that an earlier hint call showed to be free.
    let got = unsafe { mmap(addr as usize as *mut c_void, len as usize, PROT_NONE, flags, -1, 0) };
    if got == map_failed() {
        return Err(mem_err("mmap MAP_FIXED", errno()));
    }
    Ok(())
}

/// Allocates at `addr` with `mach_vm_allocate` and `VM_FLAGS_FIXED`. It fails when the range is not free.
pub fn mach_allocate_fixed(addr: u64, len: u64) -> Result<()> {
    let mut a = addr;
    // SAFETY: `a` is a valid pointer and `VM_FLAGS_FIXED` never replaces a mapping.
    let kr = unsafe { mach_vm_allocate(mach_task_self_, &mut a, len, VM_FLAGS_FIXED) };
    if kr != 0 {
        return Err(HostError::Memory(format!("mach_vm_allocate failed, kern_return {kr}")));
    }
    Ok(())
}

/// Frees a range that `map_hint`, `map_fixed` or `mach_allocate_fixed` returned.
pub fn free_range(addr: u64, len: u64) {
    // SAFETY: the callers pass ranges they allocated and no longer use. A failure leaves a mapping, which is harmless
    // for the probe.
    unsafe { mach_vm_deallocate(mach_task_self_, addr, len) };
}

/// Anonymous memory for the copy measurement of the probe.
pub struct ScratchMap {
    ptr: *mut c_void,
    len: usize,
}

impl ScratchMap {
    /// Maps `len` bytes read and write at any address.
    pub fn new(len: usize) -> Result<Self> {
        let flags = MAP_PRIVATE | MAP_ANON;
        // SAFETY: an anonymous mapping at any address.
        let ptr = unsafe { mmap(std::ptr::null_mut(), len, PROT_RW, flags, -1, 0) };
        if ptr == map_failed() {
            return Err(mem_err("mmap", errno()));
        }
        Ok(Self { ptr, len })
    }

    /// Copies `bytes` to the start of the map.
    pub fn fill(&self, bytes: &[u8]) {
        let n = bytes.len().min(self.len);
        // SAFETY: the map is writable and `n` bytes fit.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.ptr.cast::<u8>(), n) };
    }

    /// Makes the map read and execute.
    pub fn make_executable(&self) -> Result<()> {
        // SAFETY: the range is this map.
        if unsafe { mprotect(self.ptr, self.len, PROT_RX) } != 0 {
            return Err(mem_err("mprotect", errno()));
        }
        Ok(())
    }
}

impl Drop for ScratchMap {
    fn drop(&mut self) {
        // SAFETY: the range is this map and nothing uses it after the drop.
        unsafe { munmap(self.ptr, self.len) };
    }
}

/// A stack for a host thread that runs guest code, with a guard page at the low end.
pub fn map_stack(len: u64, guard: u64) -> Result<u64> {
    let base = map_hint(0, len, PROT_RW)?;
    // SAFETY: the range is the first pages of the mapping above.
    if unsafe { mprotect(base as usize as *mut c_void, guard as usize, PROT_NONE) } != 0 {
        return Err(mem_err("mprotect", errno()));
    }
    Ok(base)
}

/// A library that `dlopen` loaded. It is never unloaded, except by `close`.
pub struct Dylib {
    handle: *mut c_void,
    path: PathBuf,
}

// SAFETY: a `dlopen` handle is a plain token that dyld accepts from any thread.
unsafe impl Send for Dylib {}
// SAFETY: as for `Send`.
unsafe impl Sync for Dylib {}

fn dl_message() -> String {
    // SAFETY: `dlerror` returns null or a C string that stays valid until the next dl call on this thread.
    let p = unsafe { dlerror() };
    if p.is_null() {
        return "unknown error".into();
    }
    // SAFETY: `p` is not null and points to a C string, see above.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

impl Dylib {
    /// Loads a library. The path is made absolute first, so that the image can be found by it afterwards.
    pub fn open(path: &Path) -> Result<Dylib> {
        let path = std::fs::canonicalize(path).map_err(|e| HostError::io(path, &e))?;
        let c = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| HostError::Load(format!("{} has a nul byte", path.display())))?;
        // SAFETY: `c` is a valid C string.
        let handle = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
        if handle.is_null() {
            return Err(HostError::Load(format!("dlopen {}: {}", path.display(), dl_message())));
        }
        Ok(Dylib { handle, path })
    }

    /// The Mach-O image that `dlopen` of this library mapped: its header address and its slide.
    ///
    /// The library has no symbols, so the image is found by its path among the images of dyld. The load commands are
    /// read from the mapped header.
    pub fn image(&self) -> Result<Image> {
        // SAFETY: `_dyld_image_count` has no preconditions.
        let count = unsafe { _dyld_image_count() };
        for i in 0..count {
            // SAFETY: `i` is below the count. The name is a C string that lives as long as the image.
            let name = unsafe { _dyld_get_image_name(i) };
            if name.is_null() {
                continue;
            }
            // SAFETY: `name` is not null and is a C string, see above.
            let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
            if std::fs::canonicalize(&*name).ok().as_deref() != Some(self.path.as_path()) {
                continue;
            }
            // SAFETY: `i` is below the count.
            let (head, slide) = unsafe { (_dyld_get_image_header(i), _dyld_get_image_vmaddr_slide(i)) };
            if head.is_null() {
                return Err(HostError::Load("dyld gave no header for the library".into()));
            }
            return Ok(Image {
                header: head as usize as u64,
                slide: slide as i64,
            });
        }
        Err(HostError::Load(format!("dyld does not list {} as a loaded image", self.path.display())))
    }

    /// Unloads the library.
    pub fn close(self) {
        // SAFETY: the handle is live and `self` is consumed.
        unsafe { dlclose(self.handle) };
    }
}

/// A Mach-O image that dyld mapped.
#[derive(Debug, Clone, Copy)]
pub struct Image {
    /// The address of the Mach-O header.
    pub header: u64,
    /// How far dyld moved the image from its preferred addresses.
    pub slide: i64,
}

impl Image {
    /// The header and the load commands, copied out of the mapping.
    pub fn commands(&self) -> Result<Vec<u8>> {
        let mut head = [0u8; macho::HEADER_SIZE];
        // SAFETY: dyld mapped the header at this address, it is readable for the life of the image.
        unsafe { std::ptr::copy_nonoverlapping(self.header as usize as *const u8, head.as_mut_ptr(), head.len()) };
        let total = macho::commands_end(&head)?;
        let mut all = vec![0u8; total];
        // SAFETY: the load commands directly follow the header in the same mapped segment.
        unsafe { std::ptr::copy_nonoverlapping(self.header as usize as *const u8, all.as_mut_ptr(), total) };
        Ok(all)
    }
}

/// Creates a pthread key with no destructor.
pub fn key_create() -> Result<usize> {
    let mut k = 0usize;
    // SAFETY: `k` is a valid pointer.
    let r = unsafe { pthread_key_create(&mut k, None) };
    if r != 0 {
        return Err(HostError::Os {
            what: "pthread_key_create",
            errno: r,
        });
    }
    Ok(k)
}

/// Sets the value of a key for the current thread.
pub fn key_set(key: usize, value: u64) -> Result<()> {
    // SAFETY: the key came from `key_create`.
    let r = unsafe { pthread_setspecific(key, value as usize as *const c_void) };
    if r != 0 {
        return Err(HostError::Os {
            what: "pthread_setspecific",
            errno: r,
        });
    }
    Ok(())
}

/// Reads the value of a key for the current thread.
pub fn key_get(key: usize) -> u64 {
    // SAFETY: the key came from `key_create`.
    unsafe { pthread_getspecific(key) as usize as u64 }
}

/// Starts the kernel of `nxkit-rt`. `thread_entry` runs every guest thread that the game creates, with the core rules
/// of Horizon on.
pub fn kernel_init(thread_entry: nxkit_rt::NxkitThreadEntry) -> Result<()> {
    let config = nxkit_rt::NxkitKernelConfig {
        thread_entry: Some(thread_entry),
        user: std::ptr::null_mut(),
        core_rules: 1,
        roles: std::ptr::null(),
        role_count: 0,
    };
    // SAFETY: `config` is valid for the call and has no rules. The guest range is registered before any svc runs.
    match unsafe { nxkit_rt::nxkit_kernel_init(&config) } {
        0 => Ok(()),
        r => Err(HostError::App(format!("nxkit_kernel_init returned {r}"))),
    }
}
