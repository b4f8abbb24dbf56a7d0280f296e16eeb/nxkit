//! `vkGetInstanceProcAddr` and `vkGetDeviceProcAddr`: name lookup with the retry of the reference layer.
//!
//! The ABI layer keeps the table of its `extern "C"` functions. [`resolve`] looks a name up in it. A name ending in
//! `KHR` that the table lacks is tried again without the suffix, for an extension that Vulkan 1.1 promoted to core
//! (`vkBindBufferMemory2KHR` is `vkBindBufferMemory2`). Every name is logged once with what it resolved to.

#[cfg(test)]
use std::collections::HashSet;

use crate::result::Log;

/// The `vk*` names that Hades (`0100535012974000`) holds: its imports and the names it asks `vk*ProcAddr` for.
pub const HADES_NAMES: &[&str] = &[
    "vkAcquireNextImageKHR",
    "vkAllocateCommandBuffers",
    "vkAllocateDescriptorSets",
    "vkAllocateMemory",
    "vkBeginCommandBuffer",
    "vkBindBufferMemory",
    "vkBindBufferMemory2",
    "vkBindBufferMemory2KHR",
    "vkBindImageMemory",
    "vkBindImageMemory2",
    "vkBindImageMemory2KHR",
    "vkCmdBeginDebugUtilsLabelEXT",
    "vkCmdBeginRenderPass",
    "vkCmdBindDescriptorSets",
    "vkCmdBindIndexBuffer",
    "vkCmdBindPipeline",
    "vkCmdBindVertexBuffers",
    "vkCmdCopyBuffer",
    "vkCmdCopyBufferToImage",
    "vkCmdDebugMarkerBeginEXT",
    "vkCmdDebugMarkerEndEXT",
    "vkCmdDrawIndexed",
    "vkCmdDrawIndexedIndirectCountAMD",
    "vkCmdDrawIndirectCountAMD",
    "vkCmdEndDebugUtilsLabelEXT",
    "vkCmdEndRenderPass",
    "vkCmdInsertDebugUtilsLabelEXT",
    "vkCmdPipelineBarrier",
    "vkCmdSetScissor",
    "vkCmdSetViewport",
    "vkCreateBuffer",
    "vkCreateBufferView",
    "vkCreateCommandPool",
    "vkCreateDebugReportCallbackEXT",
    "vkCreateDebugUtilsMessengerEXT",
    "vkCreateDescriptorPool",
    "vkCreateDescriptorSetLayout",
    "vkCreateDescriptorUpdateTemplateKHR",
    "vkCreateDevice",
    "vkCreateFence",
    "vkCreateFramebuffer",
    "vkCreateGraphicsPipelines",
    "vkCreateImage",
    "vkCreateImageView",
    "vkCreateInstance",
    "vkCreatePipelineLayout",
    "vkCreateRenderPass",
    "vkCreateSampler",
    "vkCreateSemaphore",
    "vkCreateShaderModule",
    "vkCreateSwapchainKHR",
    "vkCreateViSurfaceNN",
    "vkDebugMarkerSetObjectNameEXT",
    "vkDestroyBuffer",
    "vkDestroyBufferView",
    "vkDestroyCommandPool",
    "vkDestroyDebugReportCallbackEXT",
    "vkDestroyDebugUtilsMessengerEXT",
    "vkDestroyDescriptorPool",
    "vkDestroyDescriptorSetLayout",
    "vkDestroyDescriptorUpdateTemplateKHR",
    "vkDestroyDevice",
    "vkDestroyFence",
    "vkDestroyFramebuffer",
    "vkDestroyImage",
    "vkDestroyImageView",
    "vkDestroyInstance",
    "vkDestroyPipeline",
    "vkDestroyPipelineLayout",
    "vkDestroyRenderPass",
    "vkDestroySampler",
    "vkDestroySemaphore",
    "vkDestroyShaderModule",
    "vkDestroySurfaceKHR",
    "vkDestroySwapchainKHR",
    "vkEndCommandBuffer",
    "vkEnumerateDeviceExtensionProperties",
    "vkEnumerateDeviceLayerProperties",
    "vkEnumerateInstanceExtensionProperties",
    "vkEnumerateInstanceLayerProperties",
    "vkEnumeratePhysicalDeviceGroupsKHR",
    "vkEnumeratePhysicalDevices",
    "vkFlushMappedMemoryRanges",
    "vkFreeCommandBuffers",
    "vkFreeMemory",
    "vkGetBufferMemoryRequirements",
    "vkGetBufferMemoryRequirements2",
    "vkGetBufferMemoryRequirements2KHR",
    "vkGetDeviceProcAddr",
    "vkGetDeviceQueue",
    "vkGetFenceStatus",
    "vkGetImageMemoryRequirements",
    "vkGetImageMemoryRequirements2",
    "vkGetImageMemoryRequirements2KHR",
    "vkGetImageSparseMemoryRequirements",
    "vkGetInstanceProcAddr",
    "vkGetPhysicalDeviceFeatures2",
    "vkGetPhysicalDeviceFormatProperties",
    "vkGetPhysicalDeviceImageFormatProperties",
    "vkGetPhysicalDeviceMemoryProperties",
    "vkGetPhysicalDeviceMemoryProperties2",
    "vkGetPhysicalDeviceProperties",
    "vkGetPhysicalDeviceProperties2",
    "vkGetPhysicalDeviceQueueFamilyProperties",
    "vkGetPhysicalDeviceSurfaceCapabilitiesKHR",
    "vkGetPhysicalDeviceSurfaceFormatsKHR",
    "vkGetPhysicalDeviceSurfacePresentModesKHR",
    "vkGetPhysicalDeviceSurfaceSupportKHR",
    "vkGetSwapchainImagesKHR",
    "vkInvalidateMappedMemoryRanges",
    "vkMapMemory",
    "vkQueueBeginDebugUtilsLabelEXT",
    "vkQueueBindSparse",
    "vkQueueEndDebugUtilsLabelEXT",
    "vkQueueInsertDebugUtilsLabelEXT",
    "vkQueuePresentKHR",
    "vkQueueSubmit",
    "vkQueueWaitIdle",
    "vkResetCommandPool",
    "vkResetFences",
    "vkSetDebugUtilsObjectNameEXT",
    "vkSetDebugUtilsObjectTagEXT",
    "vkSubmitDebugUtilsMessageEXT",
    "vkUnmapMemory",
    "vkUpdateDescriptorSetWithTemplateKHR",
    "vkWaitForFences",
];

/// The names that Vulkan 1.1 has without the `KHR` suffix, the ones that the retry of [`resolve`] finds.
pub fn core_name(name: &str) -> Option<&str> {
    name.strip_suffix("KHR").filter(|n| n.len() > 2)
}

/// Looks `name` up with `find`, then without a `KHR` suffix. Gives None for a name that does not start with `vk`.
/// Every name is logged once in `log` with what it resolved to.
pub fn resolve<T>(log: &Log, name: &str, find: impl Fn(&str) -> Option<T>) -> Option<T> {
    if !name.starts_with("vk") {
        return None;
    }
    let found = find(name).or_else(|| core_name(name).and_then(&find));
    let what = if found.is_some() {
        "host implementation"
    } else {
        "none (returns null)"
    };
    log.once(
        &format!("lookup {name}"),
        format_args!("lookup {name} -> {what}"),
    );
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn khr_names_fall_back_to_core() {
        let table = ["vkBindBufferMemory2", "vkCreateSwapchainKHR"];
        let find = |n: &str| table.iter().position(|t| *t == n);
        let log = Log::default();
        assert_eq!(resolve(&log, "vkBindBufferMemory2KHR", find), Some(0));
        assert_eq!(resolve(&log, "vkCreateSwapchainKHR", find), Some(1));
        assert_eq!(resolve(&log, "vkCmdDrawIndirectCountAMD", find), None);
        assert_eq!(resolve(&log, "glClear", find), None);
        assert!(log.seen("lookup vkBindBufferMemory2KHR"));
        assert_eq!(core_name("KHR"), None);
    }

    #[test]
    fn the_hades_list_is_complete_and_unique() {
        assert_eq!(HADES_NAMES.len(), 126);
        let set: HashSet<&&str> = HADES_NAMES.iter().collect();
        assert_eq!(set.len(), HADES_NAMES.len());
        assert!(HADES_NAMES.contains(&"vkUpdateDescriptorSetWithTemplateKHR"));
    }
}
