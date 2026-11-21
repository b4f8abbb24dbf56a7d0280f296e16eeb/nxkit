//! Tests of the `extern "C"` functions over guest memory, with the recording backend of `nxkit-vk` installed once
//! for the process. Each function is called through its address in the table, as the game calls it.

use std::sync::{Arc, OnceLock};

use nxkit_abi::guest;
use nxkit_vk::desc::SamplerId;
use nxkit_vk::handle::LOADER_MAGIC;
use nxkit_vk::mem::StructBytes;
use nxkit_vk::recording::{Call, Recording};

use super::ctx::{InstallError, Settings, install, installed};
use super::procs;
use crate::nvn::abi::testing;

/// The backend of the one context of the test process.
fn backend() -> &'static Arc<Recording> {
    static REC: OnceLock<Arc<Recording>> = OnceLock::new();
    REC.get_or_init(|| {
        let rec = Arc::new(Recording::default());
        install(rec.clone(), Settings::default()).expect("install");
        rec
    })
}

/// The function of `name` in the table, as a function pointer of type `$t`.
macro_rules! vkfn {
    ($name:literal as $t:ty) => {{
        let a = procs()
            .iter()
            .find(|p| p.name == $name)
            .map(|p| p.addr)
            .expect($name);
        // SAFETY: the address is the one of the `extern "C"` function that the table lists under `$name`, and `$t` is
        // its signature.
        unsafe { std::mem::transmute::<usize, $t>(a) }
    }};
}

type Create = extern "C" fn(u64, u64, u64, u64) -> i32;
type Destroy = extern "C" fn(u64, u64, u64);

/// Guest memory with `bytes` in it.
fn put(bytes: &[u8]) -> u64 {
    let a = testing::alloc(bytes.len().max(16) as u64);
    guest::write(a, bytes).expect("write");
    a
}

fn read(addr: u64) -> u64 {
    guest::read_u64(addr).expect("read")
}

fn magic(h: u64) -> Option<u64> {
    installed().and_then(|c| c.slots.word(h))
}

/// The instance and the device, through the extern functions.
fn device() -> u64 {
    backend();
    let out = testing::alloc(16);
    let create_instance = vkfn!("vkCreateInstance" as extern "C" fn(u64, u64, u64) -> i32);
    assert_eq!(create_instance(put(&StructBytes::new(64).0), 0, out), 0);
    let create_device = vkfn!("vkCreateDevice" as Create);
    assert_eq!(
        create_device(0, put(&StructBytes::new(72).0), 0, out + 8),
        0
    );
    read(out + 8)
}

#[test]
fn a_second_install_is_refused() {
    backend();
    assert_eq!(
        install(Arc::new(Recording::default()), Settings::default()),
        Err(InstallError::AlreadyInstalled)
    );
}

#[test]
fn the_instance_and_the_device_are_slots_with_the_loader_magic() {
    let dev = device();
    assert_eq!(magic(dev), Some(LOADER_MAGIC));
    let out = testing::alloc(64);
    let enumerate = vkfn!("vkEnumeratePhysicalDevices" as extern "C" fn(u64, u64, u64) -> i32);
    assert_eq!(enumerate(0, out, 0), 0);
    assert_eq!(guest::read_u32(out).expect("count"), 1);
    assert_eq!(enumerate(0, out, out + 8), 0);
    assert_eq!(magic(read(out + 8)), Some(LOADER_MAGIC));
    let queue = vkfn!("vkGetDeviceQueue" as extern "C" fn(u64, u32, u32, u64));
    queue(dev, 0, 0, out + 16);
    assert_eq!(magic(read(out + 16)), Some(LOADER_MAGIC));
}

#[test]
fn a_sampler_round_trip_reaches_the_backend() {
    let dev = device();
    let out = testing::alloc(16);
    let create = vkfn!("vkCreateSampler" as Create);
    assert_eq!(create(dev, put(&StructBytes::new(80).0), 0, out), 0);
    let s = read(out);
    let destroy = vkfn!("vkDestroySampler" as Destroy);
    destroy(dev, s, 0);
    let calls = backend().calls();
    assert!(
        calls
            .iter()
            .any(|c| matches!(c, Call::CreateSampler(SamplerId(id), _) if *id == s))
    );
    assert!(calls.contains(&Call::DestroySampler(SamplerId(s))));
}

#[test]
fn a_buffer_gets_its_requirements_and_goes() {
    let dev = device();
    let out = testing::alloc(64);
    // VkBufferCreateInfo: size at 24, usage at 32 (transfer destination).
    let ci = put(&StructBytes::new(56).u64(24, 1000).u32(32, 0x2).0);
    assert_eq!(vkfn!("vkCreateBuffer" as Create)(dev, ci, 0, out), 0);
    let b = read(out);
    let req = vkfn!("vkGetBufferMemoryRequirements" as extern "C" fn(u64, u64, u64));
    req(dev, b, out + 8);
    assert!(read(out + 8) >= 1000);
    assert_eq!(read(out + 16), 256);
    vkfn!("vkDestroyBuffer" as Destroy)(dev, b, 0);
}

#[test]
fn a_procaddr_lookup_finds_the_core_function_of_a_khr_name() {
    let dev = device();
    let gdpa = vkfn!("vkGetDeviceProcAddr" as extern "C" fn(u64, u64) -> u64);
    let core = procs()
        .iter()
        .find(|p| p.name == "vkGetPhysicalDeviceFeatures2")
        .map_or(0, |p| p.addr as u64);
    assert_eq!(gdpa(dev, put(b"vkGetPhysicalDeviceFeatures2KHR\0")), core);
    assert_eq!(gdpa(dev, put(b"vkNoSuchFunctionKHR\0")), 0);
}

#[test]
fn command_buffers_are_submitted_and_signal_their_fence() {
    let dev = device();
    let out = testing::alloc(0x100);
    assert_eq!(vkfn!("vkCreateCommandPool" as Create)(dev, 0, 0, out), 0);
    // VkCommandBufferAllocateInfo: the pool at 16, the level at 24, the count at 28.
    let ai = put(&StructBytes::new(32).u64(16, read(out)).u32(28, 2).0);
    let alloc = vkfn!("vkAllocateCommandBuffers" as extern "C" fn(u64, u64, u64) -> i32);
    assert_eq!(alloc(dev, ai, out + 8), 0);
    let cbs = [read(out + 8), read(out + 16)];
    assert!(cbs.iter().all(|&c| magic(c) == Some(LOADER_MAGIC)));
    let begin = vkfn!("vkBeginCommandBuffer" as extern "C" fn(u64, u64) -> i32);
    let end = vkfn!("vkEndCommandBuffer" as extern "C" fn(u64) -> i32);
    let barrier = vkfn!(
        "vkCmdPipelineBarrier" as extern "C" fn(u64, u32, u32, u32, u32, u64, u32, u64, u32, u64)
    );
    for &cb in &cbs {
        assert_eq!(begin(cb, 0), 0);
        barrier(cb, 0, 0, 0, 1, 0, 0, 0, 0, 0);
        assert_eq!(end(cb), 0);
    }
    assert_eq!(vkfn!("vkCreateFence" as Create)(dev, 0, 0, out + 24), 0);
    let fence = read(out + 24);
    // VkSubmitInfo: the command buffer count at 40, the array at 48.
    let si = put(&StructBytes::new(72).u32(40, 2).u64(48, out + 8).0);
    let submit = vkfn!("vkQueueSubmit" as extern "C" fn(u64, u32, u64, u64) -> i32);
    assert_eq!(submit(0, 1, si, fence), 0);
    let wait = vkfn!("vkWaitForFences" as extern "C" fn(u64, u32, u64, u32, u64) -> i32);
    assert_eq!(wait(dev, 1, out + 24, 1, u64::MAX), 0);
    assert_eq!(
        vkfn!("vkGetFenceStatus" as extern "C" fn(u64, u64) -> i32)(dev, fence),
        0
    );
    let submitted = backend().submissions();
    assert!(submitted.iter().any(|s| {
        s.command_buffers
            .iter()
            .map(|c| c.handle)
            .collect::<Vec<_>>()
            == cbs
    }));
    let free = vkfn!("vkFreeCommandBuffers" as extern "C" fn(u64, u64, u32, u64));
    free(dev, read(out), 2, out + 8);
    assert!(cbs.iter().all(|&c| magic(c).is_none()));
}

#[test]
fn a_present_is_a_frame_of_the_vi_layer() {
    let dev = device();
    let out = testing::alloc(0x100);
    let surface = vkfn!("vkCreateViSurfaceNN" as Create);
    assert_eq!(surface(0, put(&StructBytes::new(32).0), 0, out), 0);
    // VkSwapchainCreateInfoKHR: the surface at 24, 2 images at 32, B8G8R8A8_UNORM at 36, 1280x720 at 44, the usage
    // at 56, FIFO at 88.
    let sci = StructBytes::new(104)
        .u64(24, read(out))
        .u32(32, 2)
        .u32(36, 44)
        .u32(44, 1280)
        .u32(48, 720)
        .u32(56, 0x10)
        .u32(88, 2);
    assert_eq!(
        vkfn!("vkCreateSwapchainKHR" as Create)(dev, put(&sci.0), 0, out + 8),
        0
    );
    let sc = read(out + 8);
    let images = vkfn!("vkGetSwapchainImagesKHR" as extern "C" fn(u64, u64, u64, u64) -> i32);
    assert_eq!(images(dev, sc, out + 16, 0), 0);
    assert_eq!(guest::read_u32(out + 16).expect("count"), 2);
    let acquire =
        vkfn!("vkAcquireNextImageKHR" as extern "C" fn(u64, u64, u64, u64, u64, u64) -> i32);
    assert_eq!(acquire(dev, sc, u64::MAX, 0, 0, out + 24), 0);
    guest::write_u64(out + 32, sc).expect("swapchain");
    guest::write_u32(out + 28, 7).expect("result");
    // VkPresentInfoKHR: one swapchain at 32, the swapchains at 40, the indices at 48, the results at 56.
    let pi = StructBytes::new(64)
        .u32(32, 1)
        .u64(40, out + 32)
        .u64(48, out + 24)
        .u64(56, out + 28);
    let before = crate::layer::frames().count;
    let present = vkfn!("vkQueuePresentKHR" as extern "C" fn(u64, u64) -> i32);
    assert_eq!(present(0, put(&pi.0)), 0);
    assert!(crate::layer::frames().count > before);
    assert_eq!(guest::read_u32(out + 28).expect("result"), 0);
    assert!(
        backend()
            .calls()
            .iter()
            .any(|c| matches!(c, Call::Present(p) if p.swapchain == sc))
    );
    vkfn!("vkDestroySwapchainKHR" as Destroy)(dev, sc, 0);
}
