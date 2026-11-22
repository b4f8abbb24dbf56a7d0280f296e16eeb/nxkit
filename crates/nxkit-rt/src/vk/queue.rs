//! The queue (submits and waits), fences, semaphores, events, query pools, the swapchain and the present.
//!
//! The present goes through the API-independent part of the layer chain as the NVN present does: each present is a
//! frame of the `nn::vi` layer (`crate::layer::frame_queued`, which `GetLatestFrameNumber` and `ListFrameHistory`
//! report), counted before the backend shows it. A layer crop of `nn::vi::SetLayerCrop` is not applied (the present of
//! the backend has no crop): it is reported once.

use nxkit_vk::consts::special;
use nxkit_vk::info;
use nxkit_vk::mem::Mem;
use nxkit_vk::result::{Status, VkResult, code_of};

use super::abi::{M, call, code, handle_out, ok, procs};

extern "C" fn queue_submit(_queue: u64, count: u32, submits: u64, fence: u64) -> i32 {
    code("vkQueueSubmit", |vk| {
        ok(vk.submit(&info::submits(&M, submits, count)?, fence))
    })
}

extern "C" fn queue_wait_idle(_queue: u64) -> i32 {
    code("vkQueueWaitIdle", |vk| {
        vk.wait_idle();
        Ok(Status::Success)
    })
}

extern "C" fn device_wait_idle(_device: u64) -> i32 {
    code("vkDeviceWaitIdle", |vk| {
        vk.wait_idle();
        Ok(Status::Success)
    })
}

extern "C" fn queue_bind_sparse(_queue: u64, count: u32, _infos: u64, fence: u64) -> i32 {
    code("vkQueueBindSparse", |vk| ok(vk.bind_sparse(count, fence)))
}

extern "C" fn create_fence(_device: u64, ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateFence", |vk| {
        let signaled = info::flags(&M, ci)? & special::FENCE_SIGNALED != 0;
        handle_out(out, Ok(vk.create_fence(signaled)))
    })
}

extern "C" fn destroy_fence(_device: u64, fence: u64, _alloc: u64) {
    call("vkDestroyFence", |vk| {
        vk.destroy_fence(fence);
        Ok(())
    })
}

extern "C" fn reset_fences(_device: u64, count: u32, fences: u64) -> i32 {
    code("vkResetFences", |vk| {
        vk.reset_fences(&info::handles(&M, fences, count)?);
        Ok(Status::Success)
    })
}

extern "C" fn get_fence_status(_device: u64, fence: u64) -> i32 {
    code("vkGetFenceStatus", |vk| vk.fence_status(fence))
}

extern "C" fn wait_for_fences(
    _device: u64,
    count: u32,
    fences: u64,
    all: u32,
    timeout: u64,
) -> i32 {
    code("vkWaitForFences", |vk| {
        vk.wait_for_fences(&info::handles(&M, fences, count)?, all != 0, timeout)
    })
}

extern "C" fn create_semaphore(_device: u64, _ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateSemaphore", |vk| {
        handle_out(out, Ok(vk.create_semaphore()))
    })
}

extern "C" fn destroy_semaphore(_device: u64, semaphore: u64, _alloc: u64) {
    call("vkDestroySemaphore", |vk| {
        vk.destroy_semaphore(semaphore);
        Ok(())
    })
}

extern "C" fn create_event(_device: u64, _ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateEvent", |vk| handle_out(out, Ok(vk.create_event())))
}

extern "C" fn destroy_event(_device: u64, event: u64, _alloc: u64) {
    call("vkDestroyEvent", |vk| {
        vk.destroy_event(event);
        Ok(())
    })
}

extern "C" fn get_event_status(_device: u64, event: u64) -> i32 {
    code("vkGetEventStatus", |vk| vk.event_status(event))
}

extern "C" fn set_event(_device: u64, event: u64) -> i32 {
    code("vkSetEvent", |vk| ok(vk.set_event(event, true)))
}

extern "C" fn reset_event(_device: u64, event: u64) -> i32 {
    code("vkResetEvent", |vk| ok(vk.set_event(event, false)))
}

extern "C" fn create_query_pool(_device: u64, ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateQueryPool", |vk| {
        let (ty, count) = info::query_pool(&M, ci)?;
        handle_out(out, vk.create_query_pool(ty, count))
    })
}

extern "C" fn destroy_query_pool(_device: u64, pool: u64, _alloc: u64) {
    call("vkDestroyQueryPool", |vk| {
        vk.destroy_query_pool(pool);
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)]
extern "C" fn get_query_pool_results(
    _device: u64,
    pool: u64,
    first: u32,
    count: u32,
    size: u64,
    data: u64,
    stride: u64,
    flags: u32,
) -> i32 {
    code("vkGetQueryPoolResults", |vk| {
        vk.query_pool_results(&M, pool, first, count, size, data, stride, flags)
    })
}

extern "C" fn create_swapchain(_device: u64, ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateSwapchainKHR", |vk| {
        handle_out(out, vk.create_swapchain(&info::swapchain(&M, ci)?))
    })
}

extern "C" fn destroy_swapchain(_device: u64, swapchain: u64, _alloc: u64) {
    call("vkDestroySwapchainKHR", |vk| {
        vk.destroy_swapchain(swapchain);
        Ok(())
    })
}

extern "C" fn get_swapchain_images(_device: u64, swapchain: u64, count: u64, out: u64) -> i32 {
    code("vkGetSwapchainImagesKHR", |vk| {
        nxkit_vk::write::u64_list(&M, count, out, &vk.swapchain_images(swapchain)?)
    })
}

/// `vkAcquireNextImageKHR(device, swapchain, timeout, semaphore, fence, pImageIndex)`: the semaphore needs nothing,
/// the one serial queue orders the work.
extern "C" fn acquire_next_image(
    _device: u64,
    swapchain: u64,
    timeout: u64,
    _semaphore: u64,
    fence: u64,
    index: u64,
) -> i32 {
    code("vkAcquireNextImageKHR", |vk| {
        let (status, i) = vk.acquire_next_image(swapchain, timeout, fence)?;
        M.put_u32(index, i)?;
        Ok(status)
    })
}

/// `vkQueuePresentKHR`: one frame of the vi layer, then the images of the swapchains. Each image's result goes to
/// `pResults` when the game gave it. The call gives the first failure.
extern "C" fn queue_present(_queue: u64, pi: u64) -> i32 {
    const NAME: &str = "vkQueuePresentKHR";
    let clock = super::ctx::ctx(NAME).settings.clock_ns;
    code(NAME, |vk| {
        let p = info::present(&M, pi)?;
        let [x, y, w, h] = crate::layer::layer_crop();
        if w != 0 && h != 0 {
            vk.log().once(
                "vi layer crop",
                format_args!(
                    "[RED] vi layer crop {x},{y} {w}x{h} is not applied: the Vulkan present shows the whole image"
                ),
            );
        }
        crate::layer::frame_queued(clock());
        let results = vk.present(&p.images);
        let mut first: VkResult<Status> = Ok(Status::Success);
        for (k, r) in results.into_iter().enumerate() {
            if p.results != 0 {
                M.put_u32(p.results + 4 * k as u64, code_of(ok(r)) as u32)?;
            }
            if let (Err(e), Ok(_)) = (r, &first) {
                first = Err(e);
            }
        }
        first
    })
}

pub(super) fn procs() -> Vec<super::Proc> {
    procs![
        "vkQueueSubmit" => queue_submit [big],
        "vkQueueWaitIdle" => queue_wait_idle [big],
        "vkDeviceWaitIdle" => device_wait_idle [big],
        "vkQueueBindSparse" => queue_bind_sparse,
        "vkCreateFence" => create_fence,
        "vkDestroyFence" => destroy_fence,
        "vkResetFences" => reset_fences,
        "vkGetFenceStatus" => get_fence_status,
        "vkWaitForFences" => wait_for_fences [big],
        "vkCreateSemaphore" => create_semaphore,
        "vkDestroySemaphore" => destroy_semaphore,
        "vkCreateEvent" => create_event,
        "vkDestroyEvent" => destroy_event,
        "vkGetEventStatus" => get_event_status,
        "vkSetEvent" => set_event,
        "vkResetEvent" => reset_event,
        "vkCreateQueryPool" => create_query_pool [big],
        "vkDestroyQueryPool" => destroy_query_pool [big],
        "vkGetQueryPoolResults" => get_query_pool_results [big],
        "vkCreateSwapchainKHR" => create_swapchain [big],
        "vkDestroySwapchainKHR" => destroy_swapchain [big],
        "vkGetSwapchainImagesKHR" => get_swapchain_images,
        "vkAcquireNextImageKHR" => acquire_next_image [big],
        "vkQueuePresentKHR" => queue_present [big],
    ]
}
