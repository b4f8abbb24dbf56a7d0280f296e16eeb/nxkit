//! Native images of NVN.
//!
//! A decoded video picture is a texture: `nn::gs::NativeImageHandle`, then `nvnNativeImage`, then
//! `nvnTextureInitializeFromNativeImage` (the movies of Fire Emblem Fortune's Weave, RGBA8). The functions are not in
//! the NVN proc table. The game imports them by name from the SDK, so they are exports here.
//!
//! The handle comes from the pool of `nn::codec` ([`nxkit_svc_video::native_image`]). It keeps the picture as RGBA in
//! the memory of the game. A linear texture over a memory pool of that memory then reads each new picture without an
//! upload. The pool is made once for an image and ends with `nvnNativeImageFinalize`.
//!
//! The game gives the builder 0x100 bytes and the image 0x200 bytes. Its texture is 0x200 further on (Fire Emblem
//! Fortune's Weave). Other SDKs may reserve less, so the host writes only the first 0x10 bytes of the builder and the
//! first 0xA0 bytes of the image. The host keeps these in them:
//!
//! | object | offset | field |
//! | --- | --- | --- |
//! | builder | 0 | the device (u64) |
//! | builder | 8 | the handle (u32), then the format (i32) |
//! | image | 0 | the device, the handle, the format as in the builder |
//! | image | 0x10 | 1 when the image has a memory pool (u32) |
//! | image | 0x18 | the `NVNmemoryPool`, 0x18 bytes |
//! | image | 0x30 | scratch: a memory pool builder, 0x1C bytes |
//! | image | 0x50 | scratch: a texture builder, 0x50 bytes |
//!
//! The scratch builders are in guest memory because the NVN layer reads its builders there.

use nxkit_abi::guest;
use nxkit_svc_video::native_image;

use super::{checked, log, table};
use crate::nvn::{abi::used, pool, texture};

const OFF_DEV: u64 = 0;
const OFF_HANDLE: u64 = 8;
const OFF_FORMAT: u64 = 0xC;
const OFF_HAS_POOL: u64 = 0x10;
const OFF_POOL: u64 = 0x18;
const OFF_POOL_BUILDER: u64 = 0x30;
const OFF_TEXTURE_BUILDER: u64 = 0x50;
/// The bytes of the builder that the host writes.
const BUILDER_USED: u64 = 0x10;
/// The bytes of the image that the host writes.
const IMAGE_USED: u64 = 0xA0;
/// The bytes of the builder that the game gives, for the allocations of tests.
#[cfg(test)]
const BUILDER_SIZE: u64 = 0x100;
/// The bytes of the image that the game gives, for the allocations of tests.
#[cfg(test)]
const IMAGE_SIZE: u64 = 0x200;

const _: () = assert!(OFF_FORMAT + 4 <= BUILDER_USED);
const _: () = assert!(OFF_POOL + used::POOL <= OFF_POOL_BUILDER);
const _: () = assert!(OFF_POOL_BUILDER + used::POOL_BUILDER <= OFF_TEXTURE_BUILDER);
const _: () = assert!(OFF_TEXTURE_BUILDER + used::TEXTURE_BUILDER <= IMAGE_USED);

/// `NVN_MEMORY_POOL_FLAGS_CPU_UNCACHED | NVN_MEMORY_POOL_FLAGS_GPU_CACHED`.
const PICTURE_POOL_FLAGS: i32 = 0x2 | 0x20;

/// `nvnNativeImageBuilderSetDefaults(builder)`: native images are the host's. The game sets the device first, so it
/// stays.
extern "C" fn builder_set_defaults(b: u64) {
    const NAME: &str = "nvnNativeImageBuilderSetDefaults";
    let dev = checked(NAME, guest::read_u64(b + OFF_DEV));
    checked(NAME, guest::fill(b, BUILDER_USED, 0));
    checked(NAME, guest::write_u64(b + OFF_DEV, dev));
}

/// `nvnNativeImageBuilderSetDevice(builder, device)`: native images are the host's.
extern "C" fn builder_set_device(b: u64, dev: u64) {
    checked(
        "nvnNativeImageBuilderSetDevice",
        guest::write_u64(b + OFF_DEV, dev),
    );
}

/// `nvnNativeImageBuilderSetNativeImageHandle(builder, handle)`: native images are the host's.
extern "C" fn builder_set_handle(b: u64, handle: u32) {
    checked(
        "nvnNativeImageBuilderSetNativeImageHandle",
        guest::write_u32(b + OFF_HANDLE, handle),
    );
}

/// `nvnNativeImageBuilderSetMemoryPermission(builder, permission)`: native images are the host's, so the permission
/// has no meaning there.
extern "C" fn builder_set_memory_permission(_b: u64, _permission: u32) {}

/// `nvnNativeImageBuilderSetFormat(builder, format)`: native images are the host's.
extern "C" fn builder_set_format(b: u64, format: u32) {
    checked(
        "nvnNativeImageBuilderSetFormat",
        guest::write_u32(b + OFF_FORMAT, format),
    );
}

/// `nvnNativeImageInitialize(image, builder)`: native images are the host's. The image takes the device, the handle
/// and the format of the builder. Its memory pool is made by the first texture.
extern "C" fn image_initialize(i: u64, b: u64) -> u32 {
    const NAME: &str = "nvnNativeImageInitialize";
    let dev = checked(NAME, guest::read_u64(b + OFF_DEV));
    let handle = checked(NAME, guest::read_u32(b + OFF_HANDLE));
    let format = checked(NAME, guest::read_u32(b + OFF_FORMAT));
    checked(NAME, guest::write_u64(i + OFF_DEV, dev));
    checked(NAME, guest::write_u32(i + OFF_HANDLE, handle));
    checked(NAME, guest::write_u32(i + OFF_FORMAT, format));
    checked(NAME, guest::write_u32(i + OFF_HAS_POOL, 0));
    1
}

/// `nvnNativeImageFinalize(image)`: native images are the host's. The memory pool of the image ends with it.
extern "C" fn image_finalize(i: u64) {
    const NAME: &str = "nvnNativeImageFinalize";
    if checked(NAME, guest::read_u32(i + OFF_HAS_POOL)) != 0 {
        pool::pool_finalize(i + OFF_POOL);
    }
    checked(NAME, guest::write_u32(i + OFF_HAS_POOL, 0));
}

/// `nvnTextureInitializeFromNativeImage(texture, image)`: a linear texture over the memory of the picture. A handle
/// that names no picture fails with a log line. The memory pool of the image is made on the first call and holds the
/// whole picture memory, so later pictures of the same handle need no new pool.
extern "C" fn texture_initialize_from_native_image(t: u64, i: u64) -> u32 {
    const NAME: &str = "nvnTextureInitializeFromNativeImage";
    let dev = checked(NAME, guest::read_u64(i + OFF_DEV));
    let handle = checked(NAME, guest::read_u32(i + OFF_HANDLE));
    let format = checked(NAME, guest::read_u32(i + OFF_FORMAT));
    let Some(picture) = native_image(handle) else {
        log(format_args!(
            "nxkit-rt: warning: {NAME}: handle {handle:#x} names no picture"
        ));
        return 0;
    };
    if checked(NAME, guest::read_u32(i + OFF_HAS_POOL)) == 0 {
        let builder = i + OFF_POOL_BUILDER;
        pool::builder_set_defaults(builder);
        pool::builder_set_device(builder, dev);
        pool::builder_set_storage(builder, picture.mem, picture.size);
        pool::builder_set_flags(builder, PICTURE_POOL_FLAGS);
        if pool::pool_initialize(i + OFF_POOL, builder) == 0 {
            return 0;
        }
        checked(NAME, guest::write_u32(i + OFF_HAS_POOL, 1));
    }
    texture::initialize_linear(
        t,
        i + OFF_TEXTURE_BUILDER,
        dev,
        i + OFF_POOL,
        format as i32,
        (picture.width, picture.height, picture.stride),
    )
}

table! {
    "_Z32nvnNativeImageBuilderSetDefaultsP21NVNnativeImageBuilder" => builder_set_defaults,
    "_Z30nvnNativeImageBuilderSetDeviceP21NVNnativeImageBuilderP9NVNdevice" => builder_set_device,
    "_Z41nvnNativeImageBuilderSetNativeImageHandleP21NVNnativeImageBuilderN2nn2gs17NativeImageHandleE" => builder_set_handle,
    "_Z40nvnNativeImageBuilderSetMemoryPermissionP21NVNnativeImageBuilder30NVNnativeImageMemoryPermission" => builder_set_memory_permission,
    "_Z30nvnNativeImageBuilderSetFormatP21NVNnativeImageBuilder9NVNformat" => builder_set_format,
    "_Z24nvnNativeImageInitializeP14NVNnativeImagePK21NVNnativeImageBuilder" => image_initialize,
    "_Z22nvnNativeImageFinalizeP14NVNnativeImage" => image_finalize,
    "_Z35nvnTextureInitializeFromNativeImageP10NVNtextureP14NVNnativeImage" => texture_initialize_from_native_image,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvn::abi::testing;

    #[test]
    fn the_builder_keeps_the_device_across_set_defaults() {
        let b = testing::alloc(BUILDER_SIZE);
        builder_set_device(b, 0xD00D);
        builder_set_handle(b, 7);
        builder_set_format(b, 9);
        builder_set_defaults(b);
        assert_eq!(guest::read_u64(b).expect("device"), 0xD00D);
        assert_eq!(guest::read_u32(b + OFF_HANDLE).expect("handle"), 0);
        assert_eq!(guest::read_u32(b + OFF_FORMAT).expect("format"), 0);
    }

    #[test]
    fn an_image_takes_what_the_builder_holds_and_finalize_clears_the_pool_flag() {
        let b = testing::alloc(BUILDER_SIZE);
        let i = testing::alloc(IMAGE_SIZE);
        builder_set_device(b, 0xD00D);
        builder_set_handle(b, 0x8000_0001);
        builder_set_format(b, 37);
        guest::write_u32(i + OFF_HAS_POOL, 5).expect("flag");
        assert_eq!(image_initialize(i, b), 1);
        assert_eq!(guest::read_u64(i).expect("device"), 0xD00D);
        assert_eq!(
            guest::read_u32(i + OFF_HANDLE).expect("handle"),
            0x8000_0001
        );
        assert_eq!(guest::read_u32(i + OFF_FORMAT).expect("format"), 37);
        assert_eq!(guest::read_u32(i + OFF_HAS_POOL).expect("flag"), 0);
        image_finalize(i);
        assert_eq!(guest::read_u32(i + OFF_HAS_POOL).expect("flag"), 0);
    }

    #[test]
    fn a_handle_without_a_picture_fails() {
        let i = testing::alloc(IMAGE_SIZE);
        let t = testing::alloc(0x200);
        guest::write_u32(i + OFF_HANDLE, 0x1234_5678).expect("handle");
        assert_eq!(texture_initialize_from_native_image(t, i), 0);
    }

    #[test]
    fn the_exports_are_the_names_of_the_list() {
        let names: Vec<&str> = exports().iter().map(|e| e.name).collect();
        assert_eq!(names.len(), 8);
        assert!(names.iter().all(|n| n.contains("ative")));
    }
}
