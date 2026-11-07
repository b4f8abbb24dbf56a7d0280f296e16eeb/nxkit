//! The names of the service and the function that answers each. The names are the Switch mangled names that
//! `dm/host/hle_fs.c` and `dm/host/hle_sdkinit.c` hook.

use super::{file, mount};
use nxkit_abi::export::Export;

macro_rules! fs_exports {
    (small { $($sname:literal => $sf:path,)* } big { $($name:literal => $f:path,)* }) => {
        /// The entries that run on the stack of the calling guest thread.
        pub(super) fn entries() -> Vec<Export> {
            vec![$(Export::new($sname, $sf as *const () as usize)),*]
        }

        /// The entries that need the big stack, as `HLE_FORCE_BIG` in `hle_fs.c`.
        pub(super) fn big_entries() -> Vec<Export> {
            vec![$(Export::new($name, $f as *const () as usize)),*]
        }

        /// The names with the function that answers each, for the tests.
        #[cfg(test)]
        pub(super) const NAMES: &[(&str, &str)] = &[$(($sname, stringify!($sf)),)* $(($name, stringify!($f))),*];
    };
}

fs_exports! {
    small {
    "_ZN2nn2fs24InitializeForApplicationEv" => mount::nothing,
    }
    big {
    "_ZN2nn2fs22QueryMountRomCacheSizeEPm" => mount::query_rom_cache,
    "_ZN2nn2fs22QueryMountRomCacheSizeEPmi" => mount::query_rom_cache_index,
    "_ZN2nn2fs8MountRomEPKcPvm" => mount::mount_rom,
    "_ZN2nn2fs8MountRomEPKc" => mount::mount_rom_plain,
    "_ZN2nn2fs8MountRomEPKciPvm" => mount::mount_rom_index,
    "_ZN2nn2fs31QueryMountAddOnContentCacheSizeEPmi" => mount::query_aoc_cache,
    "_ZN2nn2fs17MountAddOnContentEPKciPvm" => mount::mount_aoc,
    "_ZN2nn2fs29QueryMountSystemDataCacheSizeEPmNS_3ncm12SystemDataIdE" => mount::query_system_data_cache,
    "_ZN2nn2fs15MountSystemDataEPKcNS_3ncm12SystemDataIdEPvm" => mount::mount_system_data,
    "_ZN2nn2fs9IsMountedEPKc" => mount::is_mounted,
    "_ZN2nn2fs13IsPathMountedEPKc" => mount::is_path_mounted,
    "_ZN2nn2fs16GetPathLengthMaxEPmPKc" => mount::get_name_length_max,
    "_ZN2nn2fs21GetEntryNameLengthMaxEPmPKc" => mount::get_name_length_max,
    "_ZN2nn2fs21GetUtf16PathLengthMaxEPbPmPKc" => mount::get_utf16_length_max,
    "_ZN2nn2fs26GetUtf16EntryNameLengthMaxEPbPmPKc" => mount::get_utf16_length_max,
    "_ZN2nn2fs21MountTemporaryStorageEPKc" => mount::mount_temp,
    "_ZN2nn2fs20OpenCacheStorageListEPNS0_22CacheStorageListHandleE" => mount::open_cache_list,
    "_ZN2nn2fs20ReadCacheStorageListEPiPNS0_16CacheStorageInfoENS0_22CacheStorageListHandleEi" => mount::read_cache_list,
    "_ZN2nn2fs21CloseCacheStorageListENS0_22CacheStorageListHandleE" => mount::void_one,
    "_ZN2nn2fs18CreateCacheStorageEill" => mount::create_cache,
    "_ZN2nn2fs17MountCacheStorageEPKci" => mount::mount_cache,
    "_ZN2nn2fs17MountCacheStorageEPKc" => mount::mount_cache_zero,
    "_ZN2nn2fs18DeleteCacheStorageEi" => mount::delete_cache,
    "_ZN2nn2fs15GetSaveDataSizeEPlS1_RKNS_7account3UidE" => mount::get_save_size,
    "_ZN2nn2fs18GetSaveDataSizeMaxEPlS1_" => mount::get_save_size_max,
    "_ZN2nn2fs19GetCacheStorageSizeEPlS1_i" => mount::get_cache_size,
    "_ZN2nn2fs16GetFreeSpaceSizeEPlPKc" => mount::get_free_space,
    "_ZN2nn2fs14ExtendSaveDataERKNS_7account3UidEll" => mount::extend_save,
    "_ZN2nn2fs14EnsureSaveDataERKNS_7account3UidE" => mount::ensure_save,
    "_ZN2nn2fs18IsSaveDataExistingENS_13ApplicationIdERKNS_7account3UidE" => mount::is_save_existing_of,
    "_ZN2nn2fs18IsSaveDataExistingERKNS_7account3UidE" => mount::is_save_existing,
    "_ZN2nn2fs21MountSaveDataReadOnlyEPKcNS_13ApplicationIdERKNS_7account3UidE" => mount::mount_save_read_only,
    "_ZN2nn2fs13MountSaveDataEPKcRKNS_7account3UidE" => mount::mount_save,
    "_ZN2nn2fs19MountDeviceSaveDataEPKc" => mount::mount_device_save,
    "_ZN2nn2fs21GetDeviceSaveDataSizeEPlS1_" => mount::get_save_size_max,
    "_ZN2nn2fs20ExtendDeviceSaveDataEll" => mount::extend_device_save,
    "_ZN2nn2fs22GetSaveDataRestoreFlagEPbPKc" => mount::get_save_restore_flag,
    "_ZN2nn2fs6CommitEPKc" => mount::commit,
    "_ZN2nn2fs6CommitEPKPKci" => mount::commit_list,
    "_ZN2nn2fs6CommitEPKcRKNS0_12CommitOptionE" => mount::commit_option,
    "_ZN2nn2fs14CommitSaveDataEPKc" => mount::commit,
    "_ZN2nn2fs7UnmountEPKc" => mount::unmount,
    "_ZN2nn2fs25EnableGlobalFileDataCacheEPvm" => mount::void_two,
    "_ZN2nn2fs29EnableIndividualFileDataCacheEPKcPvm" => mount::ok_three,
    "_ZN2nn2fs30DisableIndividualFileDataCacheEPKc" => mount::void_one,
    "_ZN2nn2fs26DisableGlobalFileDataCacheEv" => mount::nothing,
    "_ZN2nn2fs26SetPriorityOnCurrentThreadENS0_8PriorityE" => mount::void_priority,
    "_ZN2nn2fs6detail18IsEnabledAccessLogEj" => mount::zero_mask,
    "_ZN2nn2fs6detail18IsEnabledAccessLogEv" => mount::zero,
    "_ZN2nn2fs8OpenFileEPNS0_10FileHandleEPKci" => file::open_file,
    "_ZN2nn2fs8ReadFileEPmNS0_10FileHandleElPvm" => file::read_file,
    "_ZN2nn2fs8ReadFileENS0_10FileHandleElPvm" => file::read_file_exact,
    "_ZN2nn2fs8ReadFileEPmNS0_10FileHandleElPvmRKNS0_10ReadOptionE" => file::read_file_option,
    "_ZN2nn2fs8ReadFileENS0_10FileHandleElPvmRKNS0_10ReadOptionE" => file::read_file_exact_option,
    "_ZN2nn2fs9WriteFileENS0_10FileHandleElPKvmRKNS0_11WriteOptionE" => file::write_file,
    "_ZN2nn2fs11GetFileSizeEPlNS0_10FileHandleE" => file::get_file_size,
    "_ZN2nn2fs11SetFileSizeENS0_10FileHandleEl" => file::set_file_size,
    "_ZN2nn2fs9FlushFileENS0_10FileHandleE" => file::flush_file,
    "_ZN2nn2fs9CloseFileENS0_10FileHandleE" => file::close_file,
    "_ZN2nn2fs15GetFileOpenModeENS0_10FileHandleE" => file::get_file_open_mode,
    "_ZN2nn2fs10CreateFileEPKcl" => file::create_file,
    "_ZN2nn2fs10DeleteFileEPKc" => file::delete_file,
    "_ZN2nn2fs15CreateDirectoryEPKc" => file::create_directory,
    "_ZN2nn2fs15DeleteDirectoryEPKc" => file::delete_directory,
    "_ZN2nn2fs26DeleteDirectoryRecursivelyEPKc" => file::delete_directory_recursively,
    "_ZN2nn2fs25CleanDirectoryRecursivelyEPKc" => file::clean_directory_recursively,
    "_ZN2nn2fs10RenameFileEPKcS2_" => file::rename_file,
    "_ZN2nn2fs15RenameDirectoryEPKcS2_" => file::rename_directory,
    "_ZN2nn2fs12GetEntryTypeEPNS0_18DirectoryEntryTypeEPKc" => file::get_entry_type,
    "_ZN2nn2fs13OpenDirectoryEPNS0_15DirectoryHandleEPKci" => file::open_directory,
    "_ZN2nn2fs13ReadDirectoryEPlPNS0_14DirectoryEntryENS0_15DirectoryHandleEl" => file::read_directory,
    "_ZN2nn2fs14CloseDirectoryENS0_15DirectoryHandleE" => file::close_directory,
    "_ZN2nn2fs22GetDirectoryEntryCountEPlNS0_15DirectoryHandleE" => file::get_directory_entry_count,
}
}
