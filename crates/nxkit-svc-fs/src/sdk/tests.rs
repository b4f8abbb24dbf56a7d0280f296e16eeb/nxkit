use super::table::NAMES;
use super::*;
use crate::files::{DIR_DIRECTORIES, DIR_FILES, ENTRY_SIZE, MODE_READ, MODE_WRITE};
use crate::result::{PATH_EXISTS, PATH_NOT_FOUND, TARGET_NOT_FOUND};
use crate::testutil::{Guest, global};

#[test]
fn names_are_unique_and_are_fs_names() {
    let mut all = exports();
    all.extend(big_exports());
    assert_eq!(all.len(), NAMES.len());
    let mut names: Vec<&str> = all.iter().map(|e| e.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), all.len());
    assert!(names.iter().all(|n| n.starts_with("_ZN2nn2fs")));
    assert!(all.iter().all(|e| e.addr != 0));
}

#[test]
fn the_functions_that_hle_fs_hooks_are_all_there() {
    let all = big_exports();
    assert!(exports().iter().all(|e| e.name == "_ZN2nn2fs24InitializeForApplicationEv"));
    for n in [
        "_ZN2nn2fs8MountRomEPKcPvm",
        "_ZN2nn2fs8OpenFileEPNS0_10FileHandleEPKci",
        "_ZN2nn2fs8ReadFileEPmNS0_10FileHandleElPvm",
        "_ZN2nn2fs11GetFileSizeEPlNS0_10FileHandleE",
        "_ZN2nn2fs9CloseFileENS0_10FileHandleE",
        "_ZN2nn2fs13MountSaveDataEPKcRKNS_7account3UidE",
        "_ZN2nn2fs14CommitSaveDataEPKc",
    ] {
        assert!(all.iter().any(|e| e.name == n), "{n} is missing");
    }
}

#[test]
fn the_romfs_is_mounted_opened_and_read() {
    global();
    let g = Guest::new(0x1000);
    let (rom, path, bad) = (g.cstr(0, "sdkrom"), g.cstr(0x40, "sdkrom:/a.bin"), g.cstr(0x80, "sdkrom:/nope.bin"));
    assert_eq!(mount::is_mounted(rom), 0);
    assert_eq!(mount::mount_rom(rom, 0, 0), OK);
    assert_eq!(mount::is_mounted(rom), 1);
    assert_eq!(mount::is_path_mounted(path), 1);
    assert_eq!(mount::is_path_mounted(g.cstr(0xC0, "other:/x")), 0);

    let out = g.addr() + 0x100;
    assert_eq!(file::open_file(out, bad, MODE_READ as i32), PATH_NOT_FOUND);
    assert_eq!(file::open_file(out, path, MODE_WRITE as i32), FAIL);
    assert_eq!(file::open_file(out, path, MODE_READ as i32), OK);
    let h = g.u64_at(0x100);
    assert_ne!(h, 0);

    assert_eq!(file::get_file_size(out, h), OK);
    assert_eq!(g.u64_at(0x100), 10);
    assert_eq!(file::get_file_open_mode(h), MODE_READ);
    let (buf, count) = (g.addr() + 0x200, g.addr() + 0x108);
    assert_eq!(file::read_file(count, h, 4, buf, 3), OK);
    assert_eq!(g.u64_at(0x108), 3);
    assert_eq!(g.bytes(0x200, 3), b"456");
    assert_eq!(file::read_file(count, h, 8, buf, 8), OK);
    assert_eq!(g.u64_at(0x108), 2);
    assert_eq!(file::read_file_exact(h, 0, buf, 10), OK);
    assert_eq!(g.bytes(0x200, 10), b"0123456789");
    assert_eq!(file::read_file_exact(h, 8, buf, 4), FAIL);
    assert_eq!(file::flush_file(h), OK);
    file::close_file(h);
    assert_eq!(file::read_file(count, h, 0, buf, 1), FAIL);
    assert_eq!(g.u64_at(0x108), 0);
    assert_eq!(file::read_file(count, 0, 0, buf, 1), FAIL);
    assert_eq!(file::get_file_size(out, h), FAIL);
}

#[test]
fn a_directory_of_the_romfs_is_listed() {
    global();
    let g = Guest::new(0x2000);
    assert_eq!(mount::mount_rom(g.cstr(0, "sdkrom2"), 0, 0), OK);
    let (out, count) = (g.addr() + 0x100, g.addr() + 0x108);
    let root = g.cstr(0x40, "sdkrom2:/");
    assert_eq!(file::open_directory(out, root, (DIR_DIRECTORIES | DIR_FILES) as i32), OK);
    let h = g.u64_at(0x100);
    assert_eq!(file::get_directory_entry_count(count, h), OK);
    assert_eq!(g.u64_at(0x108), 2);
    assert_eq!(file::read_directory(count, g.addr() + 0x400, h, 8), OK);
    assert_eq!(g.u64_at(0x108), 2);
    let mut seen = Vec::new();
    for i in 0..2u64 {
        let e = g.bytes(0x400 + i * ENTRY_SIZE as u64, ENTRY_SIZE);
        let name = e[..e.iter().position(|&b| b == 0).unwrap()].to_vec();
        seen.push((String::from_utf8(name).unwrap(), e[0x304]));
    }
    seen.sort();
    assert_eq!(seen, vec![("a.bin".to_string(), 1), ("dir".to_string(), 0)]);
    assert_eq!(file::read_directory(count, g.addr() + 0x400, h, 8), OK);
    assert_eq!(g.u64_at(0x108), 0);
    file::close_directory(h);
    assert_eq!(file::get_directory_entry_count(count, h), FAIL);

    let ty = g.addr() + 0x110;
    assert_eq!(file::get_entry_type(ty, g.cstr(0x50, "sdkrom2:/dir")), OK);
    assert_eq!(g.u32_at(0x110), 0);
    assert_eq!(file::get_entry_type(ty, g.cstr(0x50, "sdkrom2:/a.bin")), OK);
    assert_eq!(g.u32_at(0x110), 1);
    assert_eq!(file::get_entry_type(ty, g.cstr(0x50, "sdkrom2:/none")), PATH_NOT_FOUND);
}

#[test]
fn add_on_content_has_a_size_and_a_mount() {
    global();
    let g = Guest::new(0x400);
    let out = g.addr() + 0x100;
    assert_eq!(mount::query_aoc_cache(out, 1), OK);
    assert!(g.u64_at(0x100) >= 0x200);
    assert_eq!(mount::query_aoc_cache(out, 9), OK);
    assert_eq!(g.u64_at(0x100), 0x1000);
    assert_eq!(mount::mount_aoc(g.cstr(0, "sdkaoc"), 1, 0, 0), OK);
    assert_eq!(mount::mount_aoc(g.cstr(0x20, "sdkaoc9"), 9, 0, 0), TARGET_NOT_FOUND);
    assert_eq!(file::open_file(out, g.cstr(0x40, "sdkaoc:/x.bin"), MODE_READ as i32), OK);
}

#[test]
fn the_queries_answer_as_nxnative() {
    let fs = global();
    let g = Guest::new(0x400);
    let (a, b) = (g.addr() + 0x100, g.addr() + 0x110);
    assert_eq!(mount::query_rom_cache(a), OK);
    assert_eq!(g.u64_at(0x100), 0x1000);
    assert_eq!(mount::get_save_size(a, b, 0), OK);
    assert_eq!((g.u64_at(0x100), g.u64_at(0x110)), (256 << 20, 256 << 20));
    assert_eq!(mount::get_save_size_max(a, 0), OK);
    assert_eq!(mount::get_name_length_max(a, 0), OK);
    assert_eq!(g.u64_at(0x100), 0x300);
    assert_eq!(mount::get_utf16_length_max(b, a, 0), OK);
    assert_eq!(mount::is_save_existing_of(fs.cfg.title_id, 0), 1);
    assert_eq!(mount::is_save_existing_of(1, 0), 0);
    assert_eq!(mount::is_save_existing(0), 1);
    assert_eq!(mount::mount_save_read_only(0, 1, 0), TARGET_NOT_FOUND);
    assert_eq!(mount::mount_system_data(0, 1, 0, 0), TARGET_NOT_FOUND);
    assert_eq!(mount::get_save_restore_flag(a, 0), OK);
    assert_eq!(mount::commit(g.cstr(0, "no-such-mount")), OK);
    assert_eq!(mount::is_mounted(0), 0);
    assert_eq!(mount::mount_rom_plain(g.addr() + 0x10_0000), FAIL);
}

#[test]
fn temporary_storage_is_written_renamed_and_deleted() {
    global();
    let g = Guest::new(0x2000);
    assert_eq!(mount::mount_temp(g.cstr(0, "sdktmp")), OK);
    let (out, path) = (g.addr() + 0x100, g.cstr(0x40, "sdktmp:/f.bin"));
    assert_eq!(file::create_file(path, 0), OK);
    assert_eq!(file::create_file(path, 0), PATH_EXISTS);
    assert_eq!(file::open_file(out, path, (MODE_READ | MODE_WRITE) as i32), OK);
    let h = g.u64_at(0x100);
    g.put(0x300, b"saved");
    assert_eq!(file::write_file(h, 0, g.addr() + 0x300, 5, 0), OK);
    assert_eq!(file::set_file_size(h, 3), OK);
    assert_eq!(file::get_file_size(out, h), OK);
    assert_eq!(g.u64_at(0x100), 3);
    assert_eq!(file::read_file_exact(h, 0, g.addr() + 0x400, 3), OK);
    assert_eq!(g.bytes(0x400, 3), b"sav");
    file::close_file(h);
    let to = g.cstr(0x80, "sdktmp:/g.bin");
    assert_eq!(file::rename_file(path, to), OK);
    assert_eq!(file::get_entry_type(out, path), PATH_NOT_FOUND);
    assert_eq!(file::create_directory(g.cstr(0xC0, "sdktmp:/d")), OK);
    assert_eq!(file::delete_directory(g.cstr(0xC0, "sdktmp:/d")), OK);
    assert_eq!(file::delete_file(to), OK);
    assert_eq!(file::delete_file(to), PATH_NOT_FOUND);
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_save_is_written_and_committed() {
    let fs = global();
    let g = Guest::new(0x1000);
    let before = fs.commit_count();
    assert_eq!(mount::mount_save(g.cstr(0, "sdksave"), 0), OK);
    assert_eq!(mount::mount_device_save(g.cstr(0x20, "sdkdevice")), OK);
    let path = g.cstr(0x40, "sdksave:/option.sav");
    assert_eq!(file::create_file(path, 4), OK);
    assert!(!fs.save_dir().join("option.sav").exists());
    assert_eq!(mount::commit(g.cstr(0, "sdksave")), OK);
    assert_eq!(std::fs::metadata(fs.save_dir().join("option.sav")).unwrap().len(), 4);
    assert_eq!(fs.commit_count(), before + 1);
    let names = g.addr() + 0x200;
    let first = g.cstr(0x300, "sdksave");
    g.put(0x200, &first.to_le_bytes());
    assert_eq!(mount::commit_list(names, 1), OK);
    assert_eq!(mount::commit_list(names, -1), FAIL);
}
