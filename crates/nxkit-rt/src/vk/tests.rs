//! Tests of the table, the gating by graphics API, `vkGet*ProcAddr`, the slot allocator and the fatal errors.

use std::collections::{HashMap, HashSet};

use nxkit_vk::handle::{Kind, LOADER_MAGIC, SLOT_BYTES, SlotAlloc};
use nxkit_vk::procaddr::HADES_NAMES;
use nxkit_vk::result::{Log, Status, VkError};

use super::abi::{ends_game, verdict};
use super::slots::{CHUNK, GuestSlots};
use super::*;
use crate::{Graphics, Service, entries_for, find_big_in, find_in};

fn by_name() -> HashMap<&'static str, usize> {
    procs().iter().map(|p| (p.name, p.addr)).collect()
}

#[test]
fn every_hades_name_has_a_function_and_no_name_is_exported_twice() {
    let mut seen = HashSet::new();
    for p in procs() {
        assert!(seen.insert(p.name), "{} is exported twice", p.name);
        assert!(p.name.starts_with("vk"), "{}", p.name);
        assert_ne!(p.addr, 0);
    }
    for n in HADES_NAMES {
        assert!(seen.contains(n), "{n} has no function");
    }
}

#[test]
fn the_vk_rows_exist_only_in_the_table_of_a_vulkan_game() {
    let (nvn, vulkan) = (entries_for(Graphics::Nvn), entries_for(Graphics::Vulkan));
    let gpu = Service::Gpu.bit();
    assert!(!nvn.iter().any(|e| e.name.starts_with("vk")));
    assert!(nvn.iter().any(|e| e.name == "nvnBootstrapLoader"));
    assert!(!vulkan.iter().any(|e| e.name == "nvnBootstrapLoader"));
    assert_eq!(crate::entries().len(), nvn.len());
    for n in HADES_NAMES {
        let row = vulkan.iter().find(|e| e.name == *n);
        assert_eq!(row.map(|e| e.service), Some(Service::Gpu), "{n}");
        let found = find_in(vulkan, gpu, n.as_bytes()).or(find_big_in(vulkan, gpu, n.as_bytes()));
        assert!(found.is_some(), "{n}");
        assert_eq!(find_in(nvn, gpu, n.as_bytes()), None, "{n}");
        assert_eq!(find_big_in(nvn, gpu, n.as_bytes()), None, "{n}");
    }
    // A big-stack function is found only as one, and the others only as plain functions.
    assert!(find_big_in(vulkan, gpu, b"vkQueueSubmit").is_some());
    assert_eq!(find_in(vulkan, gpu, b"vkQueueSubmit"), None);
    assert!(find_in(vulkan, gpu, b"vkCmdDraw").is_some());
    assert_eq!(find_big_in(vulkan, gpu, b"vkCmdDraw"), None);
    // The rows belong to the gpu service, and the API-independent vi take-overs serve both APIs.
    assert_eq!(
        find_in(vulkan, Service::all_bits() & !gpu, b"vkCmdDraw"),
        None
    );
    let vi = b"_ZN2nn2vi15GetNativeWindowEPPvPNS0_5LayerE";
    assert!(find_in(vulkan, gpu, vi).is_some() && find_in(nvn, gpu, vi).is_some());
}

#[test]
fn the_big_stack_rows_are_the_heavy_calls() {
    let big: HashSet<&str> = procs().iter().filter(|p| p.big).map(|p| p.name).collect();
    for n in [
        "vkCreateDevice",
        "vkCreateGraphicsPipelines",
        "vkCreateComputePipelines",
        "vkQueueSubmit",
        "vkQueuePresentKHR",
        "vkAcquireNextImageKHR",
        "vkUpdateDescriptorSets",
        "vkUpdateDescriptorSetWithTemplateKHR",
        "vkCreateImage",
        "vkMapMemory",
        "vkWaitForFences",
    ] {
        assert!(big.contains(n), "{n}");
    }
    for n in [
        "vkCmdDraw",
        "vkCmdSetDepthBias",
        "vkGetInstanceProcAddr",
        "vkCreateBuffer",
    ] {
        assert!(!big.contains(n), "{n}");
    }
    // A name and its KHR alias share one function, so they need one stub.
    let t = by_name();
    assert_eq!(
        t.get("vkBindBufferMemory2"),
        t.get("vkBindBufferMemory2KHR")
    );
}

#[test]
fn a_lookup_retries_without_khr_and_gives_null_for_an_unknown_name() {
    let (log, t) = (Log::default(), by_name());
    let mut stubs = HashMap::new();
    let core = t.get("vkGetPhysicalDeviceFeatures2").copied().unwrap_or(0) as u64;
    assert_ne!(core, 0);
    assert_eq!(
        find_proc(&log, &t, &stubs, "vkGetPhysicalDeviceFeatures2KHR"),
        core
    );
    assert_eq!(find_proc(&log, &t, &stubs, "vkCreateRenderPass2KHR"), 0);
    assert_eq!(find_proc(&log, &t, &stubs, "glClear"), 0);
    assert!(log.seen("lookup vkCreateRenderPass2KHR"));
    // A function with a big-stack stub is given as the stub.
    let submit = t.get("vkQueueSubmit").copied().unwrap_or(0);
    stubs.insert(submit, 0x1230);
    assert_eq!(find_proc(&log, &t, &stubs, "vkQueueSubmit"), 0x1230);
    assert_eq!(
        find_proc(&log, &t, &stubs, "vkCmdDraw"),
        t.get("vkCmdDraw").copied().unwrap_or(0) as u64
    );
}

#[test]
fn slots_carry_the_loader_magic_and_are_reused_after_a_free() {
    let s = GuestSlots::default();
    let a = s.alloc(Kind::Instance).expect("slot");
    let b = s.alloc(Kind::CommandBuffer).expect("slot");
    assert_eq!(a % 16, 0);
    assert_eq!(b, a + SLOT_BYTES);
    assert_eq!(s.word(a), Some(LOADER_MAGIC));
    s.free(a);
    s.free(a);
    s.free(0x10);
    assert_eq!(s.word(a), None);
    assert_eq!(s.alloc(Kind::Queue), Some(a));
    assert_eq!(s.word(a), Some(LOADER_MAGIC));
    // A new chunk starts when one is full.
    let more: Vec<u64> = (0..CHUNK)
        .map(|_| s.alloc(Kind::CommandBuffer).expect("slot"))
        .collect();
    let set: HashSet<u64> = more.iter().copied().collect();
    assert_eq!(set.len(), CHUNK);
    assert!(more.iter().all(|&m| s.word(m) == Some(LOADER_MAGIC)));
}

#[test]
fn a_fault_or_an_unknown_handle_ends_the_game_and_other_errors_are_codes() {
    assert!(ends_game(VkError::Unknown));
    assert!(ends_game(VkError::Fault { addr: 1, len: 4 }));
    assert!(!ends_game(VkError::OutOfDate));
    assert!(!ends_game(VkError::FormatNotSupported));
    assert_eq!(verdict("t", Err(VkError::OutOfDate)), -1_000_001_004);
    assert_eq!(verdict("t", Ok(Status::Incomplete)), 5);
    assert_eq!(verdict("t", Ok(Status::Success)), 0);
}
