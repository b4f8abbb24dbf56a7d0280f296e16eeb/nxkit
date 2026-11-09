//! Mounting and the queries about mounts: rom, add-on content, temporary and cache storage, save data and commit.

use super::*;
use crate::image::Image;
use crate::names;
use crate::result::TARGET_NOT_FOUND;
use std::sync::atomic::{AtomicBool, Ordering};

/// The cache size the SDK is told to give a mount. The host reads the folder directly and needs the minimum only.
const MIN_CACHE: u64 = 0x1000;
/// The size that save data and cache storage report, for data and journal each: more than a game asks for.
const STORAGE_SIZE: i64 = 256 << 20;
/// Horizon's limit for a path and for an entry name.
const NAME_MAX: u64 = 0x300;

pub(super) extern "C" fn query_rom_cache(out: u64) -> u32 {
    put_u64(out, MIN_CACHE)
}

pub(super) extern "C" fn query_rom_cache_index(out: u64, _index: i32) -> u32 {
    put_u64(out, MIN_CACHE)
}

pub(super) extern "C" fn mount_rom(name: u64, _cache: u64, _size: u64) -> u32 {
    run_text(name, |fs, n| fs.mount_rom(n))
}

pub(super) extern "C" fn mount_rom_plain(name: u64) -> u32 {
    run_text(name, |fs, n| fs.mount_rom(n))
}

pub(super) extern "C" fn mount_rom_index(name: u64, index: i32, _cache: u64, _size: u64) -> u32 {
    if index != 0 {
        eprintln!(
            "nxkit-svc-fs: warning: MountRom with program index {index}: the host has the romfs of this program only"
        );
    }
    run_text(name, |fs, n| fs.mount_rom(n))
}

pub(super) extern "C" fn query_aoc_cache(out: u64, index: i32) -> u32 {
    let size = crate::fs()
        .and_then(|fs| fs.aoc_dir(index))
        .and_then(|dir| match Image::build(&dir) {
            Ok(i) => Some(i.work_size()),
            Err(e) => {
                eprintln!("nxkit-svc-fs: the add-on content {index} cannot be read: {e}");
                None
            }
        })
        .unwrap_or(MIN_CACHE);
    put_u64(out, size)
}

pub(super) extern "C" fn mount_aoc(name: u64, index: i32, _cache: u64, _size: u64) -> u32 {
    let r = run_text(name, |fs, n| fs.mount_aoc(n, index));
    if r == TARGET_NOT_FOUND {
        eprintln!("nxkit-svc-fs: the add-on content {index} is not there, so the mount fails");
    }
    r
}

pub(super) extern "C" fn query_system_data_cache(out: u64, _id: u64) -> u32 {
    put_u64(out, MIN_CACHE)
}

pub(super) extern "C" fn mount_system_data(_name: u64, id: u64, _cache: u64, _size: u64) -> u32 {
    eprintln!("nxkit-svc-fs: warning: MountSystemData {id:016x}: the host has no system data");
    TARGET_NOT_FOUND
}

pub(super) extern "C" fn is_mounted(name: u64) -> u32 {
    match (text(name), crate::fs()) {
        (Ok(n), Some(fs)) => u32::from(fs.find(&n).is_some()),
        _ => 0,
    }
}

pub(super) extern "C" fn is_path_mounted(path: u64) -> u32 {
    let (Ok(p), Some(fs)) = (text(path), crate::fs()) else {
        return 0;
    };
    let p = names::fwd_slashes(&p);
    u32::from(names::split_mount(&p).is_some_and(|(n, _)| fs.find(n).is_some()))
}

pub(super) extern "C" fn get_name_length_max(out: u64, _path: u64) -> u32 {
    put_u64(out, NAME_MAX)
}

pub(super) extern "C" fn get_utf16_length_max(limited: u64, out: u64, _path: u64) -> u32 {
    if put_u8(limited, 0) != OK {
        return FAIL;
    }
    put_u64(out, NAME_MAX)
}

pub(super) extern "C" fn mount_temp(name: u64) -> u32 {
    run_text(name, |fs, n| fs.mount_temp(n))
}

pub(super) extern "C" fn open_cache_list(_handle: u64) -> u32 {
    OK
}

pub(super) extern "C" fn read_cache_list(count: u64, _info: u64, _handle: u64, _max: i32) -> u32 {
    put_u32(count, 0)
}

pub(super) extern "C" fn create_cache(index: i32, _size: i64, _journal: i64) -> u32 {
    run(|fs| fs.create_cache(index))
}

pub(super) extern "C" fn mount_cache(name: u64, index: i32) -> u32 {
    run_text(name, |fs, n| fs.mount_cache(n, index))
}

pub(super) extern "C" fn mount_cache_zero(name: u64) -> u32 {
    run_text(name, |fs, n| fs.mount_cache(n, 0))
}

pub(super) extern "C" fn delete_cache(index: i32) -> u32 {
    run(|fs| fs.delete_cache(index))
}

pub(super) extern "C" fn get_save_size(size: u64, journal: u64, _uid: u64) -> u32 {
    get_save_size_max(size, journal)
}

pub(super) extern "C" fn get_save_size_max(size: u64, journal: u64) -> u32 {
    if put_i64_opt(size, STORAGE_SIZE) != OK {
        return FAIL;
    }
    put_i64_opt(journal, STORAGE_SIZE)
}

pub(super) extern "C" fn get_cache_size(size: u64, journal: u64, _index: i32) -> u32 {
    get_save_size_max(size, journal)
}

pub(super) extern "C" fn get_free_space(out: u64, _path: u64) -> u32 {
    put_i64_opt(out, STORAGE_SIZE)
}

pub(super) extern "C" fn extend_save(_uid: u64, _size: i64, _journal: i64) -> u32 {
    OK
}

pub(super) extern "C" fn extend_device_save(_size: i64, _journal: i64) -> u32 {
    OK
}

pub(super) extern "C" fn ensure_save(_uid: u64) -> u32 {
    OK
}

pub(super) extern "C" fn is_save_existing_of(app: u64, _uid: u64) -> u32 {
    u32::from(crate::fs().is_some_and(|fs| fs.cfg.title_id == app))
}

pub(super) extern "C" fn is_save_existing(_uid: u64) -> u32 {
    1
}

pub(super) extern "C" fn mount_save_read_only(_name: u64, app: u64, _uid: u64) -> u32 {
    eprintln!(
        "nxkit-svc-fs: warning: read-only mount of the save data of program {app:016x}: the host has none"
    );
    TARGET_NOT_FOUND
}

pub(super) extern "C" fn mount_save(name: u64, _uid: u64) -> u32 {
    run_text(name, |fs, n| fs.mount_save(n, &fs.save_dir()))
}

pub(super) extern "C" fn mount_device_save(name: u64) -> u32 {
    run_text(name, |fs, n| {
        let dir = if fs.cfg.separate_device_save {
            fs.device_save_dir()
        } else {
            fs.save_dir()
        };
        fs.mount_save(n, &dir)
    })
}

pub(super) extern "C" fn get_save_restore_flag(out: u64, _name: u64) -> u32 {
    put_u8(out, 0)
}

pub(super) extern "C" fn commit(name: u64) -> u32 {
    run_text(name, |fs, n| fs.commit(n))
}

pub(super) extern "C" fn commit_option(name: u64, _option: u64) -> u32 {
    commit(name)
}

pub(super) extern "C" fn commit_list(names: u64, count: i32) -> u32 {
    let Ok(count) = u64::try_from(count) else {
        return FAIL;
    };
    let mut list = Vec::new();
    for i in 0..count {
        let Ok(p) = guest::read_u64(names + 8 * i) else {
            return FAIL;
        };
        match text(p) {
            Ok(t) => list.push(t),
            Err(r) => return r,
        }
    }
    run(|fs| fs.commit_list(&list))
}

static UNMOUNT_NOTED: AtomicBool = AtomicBool::new(false);

/// A mount of the host is permanent, so unmounting it changes nothing. A name in the SDK's own table (a RAM disk the
/// game mounted through the SDK) is released there, through the guest symbol lookup the host sets with
/// `set_guest_sym`. The SDK's `Unmount` aborts on a name it does not hold, so `IsMounted` runs first.
pub(super) extern "C" fn unmount(name: u64) {
    let (Ok(n), Some(fs)) = (text(name), crate::fs()) else {
        return;
    };
    if fs.find(&n).is_some() {
        return;
    }
    let (Some(is_mounted), Some(unmount)) = (
        crate::guest_sym("_ZN2nn2fs9IsMountedEPKc"),
        crate::guest_sym("_ZN2nn2fs7UnmountEPKc"),
    ) else {
        if !UNMOUNT_NOTED.swap(true, Ordering::Relaxed) {
            eprintln!(
                "nxkit-svc-fs: Unmount of {}: not a mount of the host, and no guest symbol lookup is set (set_guest_sym), so nothing is released",
                String::from_utf8_lossy(&n)
            );
        }
        return;
    };
    if crate::sys::release_sdk_mount(is_mounted, unmount, name) {
        eprintln!(
            "nxkit-svc-fs: Unmount of {}: in the SDK's own mount table, released there",
            String::from_utf8_lossy(&n)
        );
    }
}

pub(super) extern "C" fn nothing() {}

pub(super) extern "C" fn zero() -> u32 {
    0
}

pub(super) extern "C" fn void_one(_a: u64) {}

pub(super) extern "C" fn void_priority(_priority: i32) {}

pub(super) extern "C" fn zero_mask(_mask: u32) -> u32 {
    0
}

pub(super) extern "C" fn ok_three(_a: u64, _b: u64, _c: u64) -> u32 {
    OK
}

pub(super) extern "C" fn void_two(_a: u64, _b: u64) {}
