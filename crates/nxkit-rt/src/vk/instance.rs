//! The instance, the physical device, the device and its queue, the surface on the `nn::vi` window, `vkGet*ProcAddr`
//! and the debug extensions (`VK_EXT_debug_report`, `VK_EXT_debug_utils`, `VK_EXT_debug_marker`).
//!
//! The debug functions are no-ops. A callback or a messenger gets a valid handle that is never called, because a game
//! that enabled the extension calls its functions without checking the lookup (the reference layer met a call through
//! null in Super Mario 3D All-Stars).

use nxkit_vk::mem::Mem;
use nxkit_vk::result::{Status, VkError};
use nxkit_vk::{device, info, write};

use super::abi::{M, call, code, fatal, handle_out, ok, procs};

extern "C" fn create_instance(ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateInstance", |vk| {
        handle_out(out, vk.create_instance(&info::instance(&M, ci)?))
    })
}

extern "C" fn destroy_instance(instance: u64, _alloc: u64) {
    call("vkDestroyInstance", |vk| {
        vk.destroy_instance(instance);
        Ok(())
    })
}

extern "C" fn enumerate_instance_version(out: u64) -> i32 {
    code("vkEnumerateInstanceVersion", |_| {
        M.put_u32(out, device::API_VERSION)?;
        Ok(Status::Success)
    })
}

extern "C" fn enumerate_instance_layer_properties(count: u64, _out: u64) -> i32 {
    code("vkEnumerateInstanceLayerProperties", |_| {
        write::none(&M, count)
    })
}

extern "C" fn enumerate_instance_extension_properties(layer: u64, count: u64, out: u64) -> i32 {
    code("vkEnumerateInstanceExtensionProperties", |_| {
        if layer != 0 {
            return Err(VkError::LayerNotPresent);
        }
        write::extensions(&M, count, out, device::INSTANCE_EXTENSIONS)
    })
}

extern "C" fn enumerate_physical_devices(_instance: u64, count: u64, out: u64) -> i32 {
    code("vkEnumeratePhysicalDevices", |vk| {
        write::u64_list(&M, count, out, &vk.physical_devices())
    })
}

extern "C" fn enumerate_physical_device_groups(_instance: u64, count: u64, out: u64) -> i32 {
    code("vkEnumeratePhysicalDeviceGroups", |vk| {
        let physical = vk.physical_devices().first().copied().unwrap_or(0);
        write::device_groups(&M, count, out, physical)
    })
}

extern "C" fn get_physical_device_properties(_pd: u64, out: u64) {
    call("vkGetPhysicalDeviceProperties", |vk| {
        write::properties(&M, out, &vk.caps())
    })
}

/// `VkPhysicalDeviceProperties2`: the properties at 16. The extension structs of the chain stay as the game set them.
extern "C" fn get_physical_device_properties2(_pd: u64, out: u64) {
    call("vkGetPhysicalDeviceProperties2", |vk| {
        write::properties(&M, out + 16, &vk.caps())
    })
}

extern "C" fn get_physical_device_features(_pd: u64, out: u64) {
    call("vkGetPhysicalDeviceFeatures", |vk| {
        write::features(&M, out, &vk.caps())
    })
}

/// `VkPhysicalDeviceFeatures2`: the features at 16.
extern "C" fn get_physical_device_features2(_pd: u64, out: u64) {
    call("vkGetPhysicalDeviceFeatures2", |vk| {
        write::features(&M, out + 16, &vk.caps())
    })
}

extern "C" fn get_physical_device_memory_properties(_pd: u64, out: u64) {
    call("vkGetPhysicalDeviceMemoryProperties", |_| {
        write::memory_properties(&M, out)
    })
}

/// `VkPhysicalDeviceMemoryProperties2`: the properties at 16.
extern "C" fn get_physical_device_memory_properties2(_pd: u64, out: u64) {
    call("vkGetPhysicalDeviceMemoryProperties2", |_| {
        write::memory_properties(&M, out + 16)
    })
}

extern "C" fn get_physical_device_queue_family_properties(_pd: u64, count: u64, out: u64) {
    call("vkGetPhysicalDeviceQueueFamilyProperties", |_| {
        write::queue_families(&M, count, out).map(drop)
    })
}

extern "C" fn get_physical_device_format_properties(_pd: u64, format: u32, out: u64) {
    call("vkGetPhysicalDeviceFormatProperties", |vk| {
        write::format_properties(&M, out, device::format_properties(format, &vk.caps()))
    })
}

#[allow(clippy::too_many_arguments)]
extern "C" fn get_physical_device_image_format_properties(
    _pd: u64,
    format: u32,
    ty: u32,
    tiling: u32,
    usage: u32,
    flags: u32,
    out: u64,
) -> i32 {
    code("vkGetPhysicalDeviceImageFormatProperties", |_| {
        let p = device::image_format_properties(format, ty, tiling, usage, flags)?;
        ok(write::image_format_properties(&M, out, &p))
    })
}

/// `VkPhysicalDeviceImageFormatInfo2` (format, type, tiling, usage and flags from 16) and `VkImageFormatProperties2`
/// (the properties at 16).
extern "C" fn get_physical_device_image_format_properties2(_pd: u64, fi: u64, out: u64) -> i32 {
    code("vkGetPhysicalDeviceImageFormatProperties2", |_| {
        let p = device::image_format_properties(
            M.u32(fi + 16)?,
            M.u32(fi + 20)?,
            M.u32(fi + 24)?,
            M.u32(fi + 28)?,
            M.u32(fi + 32)?,
        )?;
        ok(write::image_format_properties(&M, out + 16, &p))
    })
}

extern "C" fn enumerate_device_extension_properties(
    _pd: u64,
    layer: u64,
    count: u64,
    out: u64,
) -> i32 {
    code("vkEnumerateDeviceExtensionProperties", |_| {
        if layer != 0 {
            return Err(VkError::LayerNotPresent);
        }
        write::extensions(&M, count, out, device::DEVICE_EXTENSIONS)
    })
}

extern "C" fn enumerate_device_layer_properties(_pd: u64, count: u64, _out: u64) -> i32 {
    code("vkEnumerateDeviceLayerProperties", |_| {
        write::none(&M, count)
    })
}

extern "C" fn create_device(_pd: u64, ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateDevice", |vk| {
        handle_out(out, vk.create_device(&info::device(&M, ci)?))
    })
}

extern "C" fn destroy_device(device: u64, _alloc: u64) {
    call("vkDestroyDevice", |vk| {
        vk.destroy_device(device);
        Ok(())
    })
}

extern "C" fn get_device_queue(_device: u64, _family: u32, _index: u32, out: u64) {
    call("vkGetDeviceQueue", |vk| M.put_u64(out, vk.queue()))
}

extern "C" fn get_device_queue2(_device: u64, _queue_info: u64, out: u64) {
    call("vkGetDeviceQueue2", |vk| M.put_u64(out, vk.queue()))
}

extern "C" fn get_instance_proc_addr(_instance: u64, name: u64) -> u64 {
    const NAME: &str = "vkGetInstanceProcAddr";
    match super::name_at(name) {
        Ok(n) => super::lookup(NAME, &n),
        Err(e) => fatal(NAME, e),
    }
}

extern "C" fn get_device_proc_addr(_device: u64, name: u64) -> u64 {
    const NAME: &str = "vkGetDeviceProcAddr";
    match super::name_at(name) {
        Ok(n) => super::lookup(NAME, &n),
        Err(e) => fatal(NAME, e),
    }
}

/// `vkCreateViSurfaceNN`: a surface on the `nn::vi` native window. The host has one window, the static object that
/// `nn::vi::GetNativeWindow` gives (`gfx::vi`). A create info without a window gets it too.
extern "C" fn create_vi_surface(_instance: u64, ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateViSurfaceNN", |vk| {
        let native = crate::gfx::vi::native_window();
        let window = match info::vi_surface_window(&M, ci)? {
            0 => native,
            w => {
                if w != native {
                    vk.log().once(
                        "vi surface window",
                        format_args!(
                            "vkCreateViSurfaceNN: window {w:#x} is not the vi native window {native:#x}; the one \
                             window of the host shows it"
                        ),
                    );
                }
                w
            }
        };
        handle_out(out, Ok(vk.create_surface(window)))
    })
}

extern "C" fn destroy_surface(_instance: u64, surface: u64, _alloc: u64) {
    call("vkDestroySurfaceKHR", |vk| {
        vk.destroy_surface(surface);
        Ok(())
    })
}

extern "C" fn get_surface_support(_pd: u64, family: u32, _surface: u64, out: u64) -> i32 {
    code("vkGetPhysicalDeviceSurfaceSupportKHR", |vk| {
        M.put_u32(out, u32::from(vk.surface_support(family)))?;
        Ok(Status::Success)
    })
}

extern "C" fn get_surface_capabilities(_pd: u64, _surface: u64, out: u64) -> i32 {
    code("vkGetPhysicalDeviceSurfaceCapabilitiesKHR", |vk| {
        ok(write::surface_caps(
            &M,
            out,
            &device::surface_caps(&vk.caps()),
        ))
    })
}

extern "C" fn get_surface_formats(_pd: u64, _surface: u64, count: u64, out: u64) -> i32 {
    code("vkGetPhysicalDeviceSurfaceFormatsKHR", |_| {
        write::surface_formats(&M, count, out)
    })
}

extern "C" fn get_surface_present_modes(_pd: u64, _surface: u64, count: u64, out: u64) -> i32 {
    code("vkGetPhysicalDeviceSurfacePresentModesKHR", |_| {
        write::u32_list(&M, count, out, &device::PRESENT_MODES)
    })
}

/// `vkCreateDebugReportCallbackEXT` and `vkCreateDebugUtilsMessengerEXT`: a token that is never called.
extern "C" fn create_debug_token(_instance: u64, _ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateDebugUtilsMessengerEXT", |vk| {
        handle_out(out, Ok(vk.create_debug_token()))
    })
}

/// `vkDestroyDebugReportCallbackEXT` and `vkDestroyDebugUtilsMessengerEXT`.
extern "C" fn destroy_debug_token(_instance: u64, token: u64, _alloc: u64) {
    call("vkDestroyDebugUtilsMessengerEXT", |vk| {
        vk.destroy_debug_token(token);
        Ok(())
    })
}

/// `vkDebugReportMessageEXT(instance, flags, objectType, object, location, messageCode, pLayerPrefix, pMessage)`.
#[allow(clippy::too_many_arguments)]
extern "C" fn debug_report_message(
    _instance: u64,
    _flags: u32,
    _ty: u32,
    _object: u64,
    _location: u64,
    _code: i32,
    _prefix: u64,
    _message: u64,
) {
    call("vkDebugReportMessageEXT", |_| Ok(()))
}

/// `vkSubmitDebugUtilsMessageEXT(instance, severity, types, pCallbackData)`.
extern "C" fn submit_debug_utils_message(_instance: u64, _severity: u32, _types: u32, _data: u64) {
    call("vkSubmitDebugUtilsMessageEXT", |_| Ok(()))
}

/// `vkSetDebugUtilsObjectNameEXT`, `vkSetDebugUtilsObjectTagEXT`, `vkDebugMarkerSetObjectNameEXT` and
/// `vkDebugMarkerSetObjectTagEXT`: `(device, pInfo)`, success.
extern "C" fn set_object_debug_info(_device: u64, _info: u64) -> i32 {
    code("vkSetDebugUtilsObjectNameEXT", |_| Ok(Status::Success))
}

/// `vkQueueBeginDebugUtilsLabelEXT` and `vkQueueInsertDebugUtilsLabelEXT`.
extern "C" fn queue_label(_queue: u64, _label: u64) {
    call("vkQueueBeginDebugUtilsLabelEXT", |_| Ok(()))
}

/// `vkQueueEndDebugUtilsLabelEXT`.
extern "C" fn queue_end_label(_queue: u64) {
    call("vkQueueEndDebugUtilsLabelEXT", |_| Ok(()))
}

/// `vkCmdBeginDebugUtilsLabelEXT`, `vkCmdInsertDebugUtilsLabelEXT`, `vkCmdDebugMarkerBeginEXT` and
/// `vkCmdDebugMarkerInsertEXT`: `(commandBuffer, pInfo)`, nothing recorded.
extern "C" fn cmd_label(cb: u64, _label: u64) {
    call("vkCmdBeginDebugUtilsLabelEXT", |vk| {
        vk.cmd_debug_label(cb);
        Ok(())
    })
}

/// `vkCmdEndDebugUtilsLabelEXT` and `vkCmdDebugMarkerEndEXT`.
extern "C" fn cmd_end_label(cb: u64) {
    call("vkCmdEndDebugUtilsLabelEXT", |vk| {
        vk.cmd_debug_label(cb);
        Ok(())
    })
}

pub(super) fn procs() -> Vec<super::Proc> {
    procs![
        "vkCreateInstance" => create_instance,
        "vkDestroyInstance" => destroy_instance,
        "vkEnumerateInstanceVersion" => enumerate_instance_version,
        "vkEnumerateInstanceLayerProperties" => enumerate_instance_layer_properties,
        "vkEnumerateInstanceExtensionProperties" => enumerate_instance_extension_properties,
        "vkEnumeratePhysicalDevices" => enumerate_physical_devices,
        "vkEnumeratePhysicalDeviceGroups" => enumerate_physical_device_groups,
        "vkEnumeratePhysicalDeviceGroupsKHR" => enumerate_physical_device_groups,
        "vkGetPhysicalDeviceProperties" => get_physical_device_properties,
        "vkGetPhysicalDeviceProperties2" => get_physical_device_properties2,
        "vkGetPhysicalDeviceFeatures" => get_physical_device_features,
        "vkGetPhysicalDeviceFeatures2" => get_physical_device_features2,
        "vkGetPhysicalDeviceMemoryProperties" => get_physical_device_memory_properties,
        "vkGetPhysicalDeviceMemoryProperties2" => get_physical_device_memory_properties2,
        "vkGetPhysicalDeviceQueueFamilyProperties" => get_physical_device_queue_family_properties,
        "vkGetPhysicalDeviceFormatProperties" => get_physical_device_format_properties,
        "vkGetPhysicalDeviceImageFormatProperties" => get_physical_device_image_format_properties,
        "vkGetPhysicalDeviceImageFormatProperties2" => get_physical_device_image_format_properties2,
        "vkEnumerateDeviceExtensionProperties" => enumerate_device_extension_properties,
        "vkEnumerateDeviceLayerProperties" => enumerate_device_layer_properties,
        "vkCreateDevice" => create_device [big],
        "vkDestroyDevice" => destroy_device,
        "vkGetDeviceQueue" => get_device_queue,
        "vkGetDeviceQueue2" => get_device_queue2,
        "vkGetInstanceProcAddr" => get_instance_proc_addr,
        "vkGetDeviceProcAddr" => get_device_proc_addr,
        "vkCreateViSurfaceNN" => create_vi_surface,
        "vkDestroySurfaceKHR" => destroy_surface,
        "vkGetPhysicalDeviceSurfaceSupportKHR" => get_surface_support,
        "vkGetPhysicalDeviceSurfaceCapabilitiesKHR" => get_surface_capabilities,
        "vkGetPhysicalDeviceSurfaceFormatsKHR" => get_surface_formats,
        "vkGetPhysicalDeviceSurfacePresentModesKHR" => get_surface_present_modes,
        "vkCreateDebugReportCallbackEXT" => create_debug_token,
        "vkDestroyDebugReportCallbackEXT" => destroy_debug_token,
        "vkDebugReportMessageEXT" => debug_report_message,
        "vkCreateDebugUtilsMessengerEXT" => create_debug_token,
        "vkDestroyDebugUtilsMessengerEXT" => destroy_debug_token,
        "vkSubmitDebugUtilsMessageEXT" => submit_debug_utils_message,
        "vkSetDebugUtilsObjectNameEXT" => set_object_debug_info,
        "vkSetDebugUtilsObjectTagEXT" => set_object_debug_info,
        "vkDebugMarkerSetObjectNameEXT" => set_object_debug_info,
        "vkDebugMarkerSetObjectTagEXT" => set_object_debug_info,
        "vkQueueBeginDebugUtilsLabelEXT" => queue_label,
        "vkQueueInsertDebugUtilsLabelEXT" => queue_label,
        "vkQueueEndDebugUtilsLabelEXT" => queue_end_label,
        "vkCmdBeginDebugUtilsLabelEXT" => cmd_label,
        "vkCmdInsertDebugUtilsLabelEXT" => cmd_label,
        "vkCmdEndDebugUtilsLabelEXT" => cmd_end_label,
        "vkCmdDebugMarkerBeginEXT" => cmd_label,
        "vkCmdDebugMarkerInsertEXT" => cmd_label,
        "vkCmdDebugMarkerEndEXT" => cmd_end_label,
    ]
}
