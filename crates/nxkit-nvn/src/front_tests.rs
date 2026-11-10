use crate::cmdbuf::CommandHandle;
use crate::descriptors::Descriptors;
use crate::error::NvnError;
use crate::frame::{Cmd, DrawState, MAX_COLOR};
use crate::front::{FlushReason, FlushStats, FrontEnd, MemoryCallback};
use crate::layout::{Target, TextureDesc};
use crate::mem::VecMem;
use crate::passes::{LoadAction, StoreAction};
use crate::record::Record;
use crate::resource::{Residency, TexId};
use crate::space::LinearSpace;
use crate::tic::{TextureView, Tsc};

pub(crate) const HOST: u64 = 0x7000_0000;
pub(crate) const GPU: u64 = 0x15_0400_8000;
const SIZE: u64 = 16 << 20;
pub(crate) const CMD: u64 = HOST + 0x80_0000;

pub(crate) type Fe = FrontEnd<LinearSpace, VecMem>;

pub(crate) fn fe() -> Fe {
    FrontEnd::new(
        LinearSpace::new(HOST, GPU, SIZE).unwrap(),
        VecMem::new(HOST, SIZE as usize),
    )
}

fn rgba() -> TextureDesc {
    TextureDesc {
        target: Target::D2,
        format: 0x25,
        flags: 0,
        samples: 0,
        width: 64,
        height: 64,
        depth: 1,
        levels: 1,
        stride: 0,
        packaged_block: None,
    }
}

pub(crate) fn targets(f: &Fe, color: &[TexId]) -> Record {
    let mut c = [0u64; MAX_COLOR];
    for (i, t) in color.iter().enumerate() {
        c[i] = f.texture_address(*t).unwrap();
    }
    Record::SetTargets { color: c, depth: 0 }
}

// Records the commands into the block at `at` and gives the handle.
pub(crate) fn record(f: &mut Fe, at: u64, cmds: &[Record]) -> CommandHandle {
    f.add_command_memory(0, at, 512).unwrap();
    f.begin_recording(0).unwrap();
    for c in cmds {
        f.write(0, *c).unwrap();
    }
    f.end_recording(0).unwrap()
}

pub(crate) fn tex(f: &mut Fe, at: u64) -> TexId {
    f.create_texture(rgba(), HOST + at).unwrap()
}

pub(crate) fn with_pools(f: &mut Fe) {
    f.add_texture_pool(0, 16, HOST + 0x30_0000).unwrap();
    f.add_sampler_pool(0, 4, HOST + 0x31_0000).unwrap();
    f.register_sampler(0, Tsc::decode(&[0; 8])).unwrap();
}

pub(crate) fn bind(slot: u8, id: u32) -> Record {
    Record::BindTexture {
        slot,
        handle: Descriptors::handle(id, 0),
    }
}

#[test]
fn a_texture_gets_the_metal_address_of_its_memory() {
    let mut f = fe();
    let t = tex(&mut f, 0x1_0000);
    assert_eq!(f.resources().get(t).unwrap().address.get(), GPU + 0x1_0000);
    assert!(f.create_texture(rgba(), HOST + SIZE).is_err());
    assert!(f.create_texture(rgba(), HOST - 4096).is_err());
}

#[test]
fn one_frame_from_records_to_plan() {
    let mut f = fe();
    with_pools(&mut f);
    let scene = tex(&mut f, 0x10_0000);
    let out = tex(&mut f, 0x20_0000);
    f.register_texture(1, scene, TextureView::identity())
        .unwrap();
    let (t_scene, t_out) = (targets(&f, &[scene]), targets(&f, &[out]));
    let present = Record::Present(f.texture_address(out).unwrap());
    let h = record(
        &mut f,
        CMD,
        &[
            t_scene,
            Record::ClearColor { slot: 0 },
            Record::Draw,
            t_out,
            bind(0, 1),
            Record::Draw,
            present,
        ],
    );
    f.begin_frame();
    f.submit(h).unwrap();
    let c = &f.frame().cmds;
    assert!(matches!(
        c[1],
        Cmd::ClearColor {
            slot: 0,
            full: true
        }
    ));
    assert!(matches!(c[4], Cmd::Draw { .. }) && matches!(c[5], Cmd::Present(t) if t == out));
    let plan = f.end_frame(FlushReason::Present).unwrap();
    assert_eq!(plan.stats.render_passes, 2);
    assert_eq!(plan.attachments[0].load, LoadAction::Clear);
    assert_eq!(plan.attachments[0].store, StoreAction::Store);
    assert!(!plan.attachments[0].store_assumed);
    assert_eq!(plan.attachments[1].load, LoadAction::Load);
}

#[test]
fn a_recording_that_is_submitted_every_frame_is_decoded_once() {
    let mut f = fe();
    let c = tex(&mut f, 0x10_0000);
    let t_c = targets(&f, &[c]);
    let h = record(
        &mut f,
        CMD,
        &[t_c, Record::ClearColor { slot: 0 }, Record::Draw],
    );
    for _ in 0..3 {
        f.begin_frame();
        f.submit(h).unwrap();
        f.submit(h).unwrap();
        assert_eq!(f.end_frame(FlushReason::Present).unwrap().stats.passes, 2);
    }
    assert_eq!((f.cache_stats().misses, f.cache_stats().hits), (1, 5));
    assert_eq!(
        f.flushes(),
        FlushStats {
            present: 3,
            cpu_wait: 0
        }
    );
    assert!(f.submit(CommandHandle(CMD + 0x1000)).is_err());
}

#[test]
fn a_called_recording_gets_one_summary_for_each_inherited_binding() {
    let mut f = fe();
    with_pools(&mut f);
    let (a, b) = (tex(&mut f, 0x10_0000), tex(&mut f, 0x20_0000));
    f.register_texture(1, a, TextureView::identity()).unwrap();
    f.register_texture(2, b, TextureView::identity()).unwrap();
    let child = record(&mut f, CMD, &[Record::Draw]);
    let pa = record(&mut f, CMD + 0x1000, &[bind(0, 1), Record::Call(child.0)]);
    let pb = record(&mut f, CMD + 0x2000, &[bind(0, 2), Record::Call(child.0)]);
    f.begin_frame();
    for h in [pa, pb, pa] {
        f.submit(h).unwrap();
    }
    let reads: Vec<_> = f
        .frame()
        .cmds
        .iter()
        .map(|c| match c {
            Cmd::Draw { reads, .. } => f.frame().list(*reads).to_vec(),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(reads, [vec![a], vec![b], vec![a]]);
    assert!(f.cache().entry(child.0, 1).is_some() && f.cache().entry(child.0, 2).is_none());
    // the child is decoded twice, the third submit hits the parent
    assert_eq!((f.cache_stats().misses, f.cache_stats().hits), (4, 1));
}

#[test]
fn the_key_holds_the_needed_state_only() {
    let mut f = fe();
    with_pools(&mut f);
    let (a, b) = (tex(&mut f, 0x10_0000), tex(&mut f, 0x20_0000));
    f.register_texture(1, a, TextureView::identity()).unwrap();
    // the child sets slot 0 and the draw state itself, but a draw needs the targets and the other slots
    let child = record(
        &mut f,
        CMD,
        &[
            bind(0, 1),
            Record::BindState(DrawState::default()),
            Record::Draw,
        ],
    );
    let t_b = targets(&f, &[b]);
    let p1 = record(&mut f, CMD + 0x1000, &[Record::Call(child.0)]);
    let p2 = record(&mut f, CMD + 0x2000, &[t_b, Record::Call(child.0)]);
    let p3 = record(&mut f, CMD + 0x3000, &[t_b, Record::Call(child.0)]);
    f.begin_frame();
    for h in [p1, p2, p3] {
        f.submit(h).unwrap();
    }
    // no targets and targets b are two keys, p3 has the keys of p2 and hits
    assert!(f.cache().entry(child.0, 1).is_some() && f.cache().entry(child.0, 2).is_none());
    assert_eq!(f.cache_stats().hits, 1);
}

#[test]
fn state_that_a_child_sets_is_visible_to_the_caller() {
    let mut f = fe();
    with_pools(&mut f);
    let a = tex(&mut f, 0x10_0000);
    f.register_texture(1, a, TextureView::identity()).unwrap();
    let child = record(&mut f, CMD, &[bind(3, 1)]);
    let parent = record(&mut f, CMD + 0x1000, &[Record::Call(child.0), Record::Draw]);
    f.begin_frame();
    f.submit(parent).unwrap();
    let Cmd::Draw { reads, .. } = f.frame().cmds[0] else {
        panic!()
    };
    assert_eq!(f.frame().list(reads), [a]);
}

#[test]
fn rewriting_command_memory_drops_the_summary_and_the_callers() {
    let mut f = fe();
    let child = record(&mut f, CMD, &[Record::Draw]);
    let parent = record(&mut f, CMD + 0x1000, &[Record::Call(child.0)]);
    f.begin_frame();
    f.submit(parent).unwrap();
    assert!(matches!(f.frame().cmds[0], Cmd::Draw { .. }));
    // the game records something else into the memory of the child
    let again = record(&mut f, CMD, &[Record::Dispatch]);
    assert_eq!(again, child);
    assert_eq!(f.cache_stats().invalidations, 2);
    assert!(f.cache().entry(parent.0, 0).is_none());
    f.begin_frame();
    f.submit(parent).unwrap();
    assert!(matches!(f.frame().cmds[0], Cmd::Dispatch { .. }));
}

#[test]
fn a_write_by_the_guest_into_command_memory_drops_the_summary() {
    let mut f = fe();
    let h = record(&mut f, CMD, &[Record::Draw]);
    f.submit(h).unwrap();
    assert_eq!(f.cache().handles(), 1);
    f.guest_wrote(CMD + 0x10_0000, 8).unwrap();
    assert_eq!(f.cache().handles(), 1);
    f.guest_wrote(CMD + 8, 8).unwrap();
    assert_eq!(f.cache().handles(), 0);
    assert_eq!(f.cache_stats().invalidations, 1);
}

#[test]
fn a_new_descriptor_for_a_bound_handle_makes_the_summary_decode_again() {
    let mut f = fe();
    with_pools(&mut f);
    let (a, b) = (tex(&mut f, 0x10_0000), tex(&mut f, 0x20_0000));
    f.register_texture(1, a, TextureView::identity()).unwrap();
    let h = record(&mut f, CMD, &[bind(0, 1), Record::Draw]);
    f.submit(h).unwrap();
    f.register_texture(1, b, TextureView::identity()).unwrap();
    f.begin_frame();
    f.submit(h).unwrap();
    assert_eq!(f.cache_stats().revalidations, 1);
    let Cmd::Draw { reads, .. } = f.frame().cmds[0] else {
        panic!()
    };
    assert_eq!(f.frame().list(reads), [b]);
}

#[test]
fn a_handle_without_a_descriptor_binds_nothing_and_is_counted() {
    let mut f = fe();
    with_pools(&mut f);
    let t = tex(&mut f, 0x10_0000);
    f.register_texture(0, t, TextureView::identity()).unwrap();
    let h = record(&mut f, CMD, &[bind(0, 0), bind(1, 9), Record::Draw]);
    f.submit(h).unwrap();
    let Cmd::Draw { reads, .. } = f.frame().cmds[0] else {
        panic!()
    };
    assert_eq!(f.frame().list(reads), [t]);
    assert_eq!(f.cache_stats().unresolved, 1);
    // A finalized texture stays readable through its slot, as the header copy in the pool does on the hardware.
    assert!(!f.finalize_texture(t).unwrap());
    f.begin_frame();
    f.submit(h).unwrap();
    // Nothing changed for the recording, so the cached decode stands and counts nothing new.
    assert_eq!(f.cache_stats().unresolved, 1);
    // Registering another texture in the slot lets the finalized one go.
    let u = tex(&mut f, 0x20_0000);
    f.register_texture(0, u, TextureView::identity()).unwrap();
    assert_eq!(f.take_dropped(), [t]);
}

#[test]
fn command_memory_is_counted_and_a_full_block_asks_for_more() {
    let mut f = fe();
    f.add_command_memory(0, CMD, 64).unwrap();
    f.begin_recording(0).unwrap();
    for _ in 0..6 {
        f.write(0, Record::Draw).unwrap();
    }
    assert_eq!(
        (f.command_memory_used(0), f.command_memory_free(0)),
        (48, 16)
    );
    assert_eq!(
        f.write(0, Record::Draw),
        Err(NvnError::OutOfCommandMemory { need: 24 })
    );
    f.add_command_memory(0, CMD + 0x1000, 64).unwrap();
    f.write(0, Record::Draw).unwrap();
    let h = f.end_recording(0).unwrap();
    f.submit(h).unwrap();
    assert_eq!(f.frame().cmds.len(), 7);
}

#[test]
fn a_recording_that_calls_itself_is_an_error() {
    let mut f = fe();
    let h = record(&mut f, CMD, &[Record::Call(CMD)]);
    assert_eq!(
        f.submit(h),
        Err(NvnError::BadRecord("calls are nested too deep"))
    );
}

#[test]
fn frame_work_counts_submitted_work_at_once_and_restarts_after_a_flush() {
    let mut f = fe();
    assert_eq!(f.frame_work(), 0);
    // The commands of a submit are in the frame as soon as the submit returns, not at the flush.
    let a = record(&mut f, CMD, &[Record::Draw, Record::Draw, Record::Barrier(1)]);
    f.submit(a).unwrap();
    assert_eq!(f.frame_work(), 2);
    assert!(!f.frame_is_empty());
    let b = record(&mut f, CMD + 0x1000, &[Record::Draw]);
    f.submit(b).unwrap();
    f.submit(a).unwrap();
    assert_eq!(f.frame_work(), 5);
    f.end_frame(FlushReason::CpuWait).unwrap();
    f.begin_frame();
    assert_eq!(f.frame_work(), 0);
    f.submit(b).unwrap();
    assert_eq!(f.frame_work(), 1);
}

#[test]
fn flushes_are_counted_by_reason() {
    let mut f = fe();
    f.end_frame(FlushReason::CpuWait).unwrap();
    f.begin_frame();
    f.end_frame(FlushReason::CpuWait).unwrap();
    f.end_frame(FlushReason::Present).unwrap();
    assert_eq!(
        f.flushes(),
        FlushStats {
            present: 1,
            cpu_wait: 2
        }
    );
}

#[test]
fn stable_recordings_without_blocking_commands_are_icb_candidates() {
    let mut f = fe();
    with_pools(&mut f);
    let a = tex(&mut f, 0x10_0000);
    f.register_texture(1, a, TextureView::identity()).unwrap();
    let model = record(&mut f, CMD, &[bind(0, 1), Record::Draw, Record::Draw]);
    let compute = record(&mut f, CMD + 0x1000, &[Record::Draw, Record::Dispatch]);
    let scene = record(&mut f, CMD + 0x2000, &[Record::Call(model.0), Record::Draw]);
    for _ in 0..4 {
        f.begin_frame();
        for h in [model, compute, scene] {
            f.submit(h).unwrap();
        }
    }
    let c = f.cache().icb_candidates();
    let handles: Vec<u64> = c.iter().map(|x| x.0).collect();
    assert_eq!(handles, [model.0, scene.0]);
    assert_eq!(c[0].1, 2);
    assert_eq!(c[1].1, 3);
}

#[test]
fn a_cpu_write_to_texture_memory_is_found() {
    let mut f = fe();
    let t = tex(&mut f, 0x10_0000);
    f.resources_mut().synced(t).unwrap();
    assert!(f.guest_wrote(HOST + 0x20_0000, 64).unwrap().is_empty());
    assert_eq!(f.guest_wrote(HOST + 0x10_0100, 64).unwrap(), [t]);
    assert_eq!(
        f.resources().get(t).unwrap().residency,
        Residency::GuestNewer
    );
    assert!(f.guest_wrote(HOST - 1, 64).is_err());
}

#[derive(Debug)]
struct Blocks {
    next: u64,
    len: u64,
    asked: Vec<(usize, u32)>,
}

impl MemoryCallback for Blocks {
    fn command_memory(&mut self, cb: usize, needed: u32) -> Option<(u64, u64)> {
        self.asked.push((cb, needed));
        let at = self.next;
        self.next += 0x1000;
        (self.len > 0).then_some((at, self.len))
    }
}

#[test]
fn the_front_end_calls_the_memory_callback_and_retries() {
    let mut f = fe();
    f.set_memory_callback(
        0,
        Some(Box::new(Blocks {
            next: CMD + 0x1000,
            len: 64,
            asked: vec![],
        })),
    );
    f.add_command_memory(0, CMD, 64).unwrap();
    f.begin_recording(0).unwrap();
    for _ in 0..20 {
        f.write(0, Record::Draw).unwrap();
    }
    let h = f.end_recording(0).unwrap();
    f.submit(h).unwrap();
    assert_eq!(f.frame().cmds.len(), 20);
}

#[test]
fn without_a_usable_callback_the_error_stays() {
    let mut f = fe();
    f.add_command_memory(0, CMD, 64).unwrap();
    f.begin_recording(0).unwrap();
    for _ in 0..6 {
        f.write(0, Record::Draw).unwrap();
    }
    let full = Err(NvnError::OutOfCommandMemory { need: 24 });
    assert_eq!(f.write(0, Record::Draw), full);
    // a callback that gives nothing, and one that is disabled
    f.set_memory_callback(
        0,
        Some(Box::new(Blocks {
            next: CMD + 0x1000,
            len: 0,
            asked: vec![],
        })),
    );
    assert_eq!(f.write(0, Record::Draw), full);
    f.set_memory_callback(
        0,
        Some(Box::new(Blocks {
            next: CMD + 0x1000,
            len: 64,
            asked: vec![],
        })),
    );
    f.set_memory_callback_enabled(0, false);
    assert_eq!(f.write(0, Record::Draw), full);
    f.set_memory_callback_enabled(0, true);
    f.write(0, Record::Draw).unwrap();
}

#[test]
fn a_callback_that_gives_too_little_memory_is_not_asked_forever() {
    let mut f = fe();
    f.set_memory_callback(
        0,
        Some(Box::new(Blocks {
            next: CMD + 0x1000,
            len: 64,
            asked: vec![],
        })),
    );
    f.add_command_memory(0, CMD, 64).unwrap();
    f.begin_recording(0).unwrap();
    let big = Record::SetTargets {
        color: [GPU; MAX_COLOR],
        depth: GPU,
    };
    // 88 bytes and 16 for a jump do not fit in blocks of 64 bytes
    assert!(matches!(
        f.write(0, big),
        Err(NvnError::OutOfCommandMemory { .. })
    ));
}
