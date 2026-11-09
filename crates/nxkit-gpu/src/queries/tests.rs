use super::*;
use nxkit_nvn::analyse::Analyser;
use nxkit_nvn::counters::{INPUT_VERTICES, SAMPLES_PASSED};
use nxkit_nvn::frame::{CmdStream, DrawState, RtSet, Span};
use nxkit_nvn::marks::{Mark, MarkOp};
use nxkit_nvn::rendercond::RenderConds;
use nxkit_nvn::resource::{ResourceTable, TexId};

fn draw() -> Cmd {
    Cmd::Draw {
        state: DrawState {
            color_write: 1,
            ..Default::default()
        },
        reads: Span::default(),
    }
}

fn arrays(count: u32, instances: u32) -> DrawArgs {
    DrawArgs::Arrays {
        first: 0,
        count,
        instances,
        base_instance: 0,
    }
}

/// A render pass of three draws (commands 1 to 3). The samples of draws 0 and 1 are counted between reports at
/// `addr` and `addr + 16`; the vertices up to draw 0 are reported at `addr + 32`, between them.
fn frame(addr: u64) -> Frame {
    let mut s = CmdStream::new();
    let mut rt = RtSet::default();
    rt.color[0] = Some(TexId(0));
    s.cmds.push(Cmd::SetRenderTargets(rt));
    s.cmds.extend([draw(), draw(), draw()]);
    let samples = |addr| MarkOp::ReportCounter {
        addr,
        kind: SAMPLES_PASSED,
    };
    let reset = MarkOp::ResetCounter {
        kind: SAMPLES_PASSED,
    };
    let vertices = MarkOp::ReportCounter {
        addr: addr + 32,
        kind: INPUT_VERTICES,
    };
    let marks = [
        (1, reset),
        (1, samples(addr)),
        (2, vertices),
        (3, samples(addr + 16)),
    ];
    s.marks.extend(marks.map(|(at, op)| Mark { at, op }));
    let mut f = Frame::new();
    f.append(&s);
    RenderConds::new().apply(1, 0, &s.marks, &mut f, |_| None);
    f
}

fn plan(f: &Frame) -> PassPlan {
    let mut table = ResourceTable::new();
    let desc = nxkit_nvn::layout::TextureDesc {
        target: nxkit_nvn::layout::Target::D2,
        format: 0x25,
        flags: 0,
        samples: 0,
        width: 4,
        height: 4,
        depth: 1,
        levels: 1,
        stride: 0,
        packaged_block: None,
    };
    table
        .create(desc, nxkit_nvn::space::GpuAddr::new(0x10_0000).unwrap())
        .unwrap();
    let mut plan = PassPlan::default();
    Analyser::new().analyse(f, &table, &mut plan).unwrap();
    plan
}

fn detail() -> FrameDetail {
    let mut d = FrameDetail::default();
    for args in [arrays(3, 1), arrays(6, 2), arrays(9, 1)] {
        let mut x = crate::walk::empty_draw();
        x.args = args;
        d.draws.push(Arc::new(x));
    }
    d
}

/// The CPU space over a fresh guest memory. Its addresses are virtual: they start at `GUEST_VA_BASE`, not at the
/// address of the Metal buffer.
fn space(ctx: &Context, len: u64) -> GuestSpace {
    let mem = Arc::new(nxkit_metal::GuestMemory::allocate(ctx, len).unwrap());
    GuestSpace::over(&mem).unwrap()
}

fn counter(guest: &GuestSpace, off: u64) -> (u64, u64) {
    let mut b = [0u8; 16];
    guest.read(off, &mut b).unwrap();
    let w = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    (w(0), w(8))
}

#[test]
fn the_vertices_of_a_draw_come_from_its_arguments() {
    assert_eq!(vertices(&arrays(6, 2)), Some(12));
    assert_eq!(vertices(&arrays(6, 0)), Some(6));
    assert_eq!(vertices(&DrawArgs::ArraysIndirect { indirect: 0x40 }), None);
}

#[test]
fn counted_draws_get_offsets_of_their_pass_and_land_once_all_were_encoded() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let guest = space(&ctx, 0x1000);
    let map = guest.address_map();
    assert_eq!(map.gpu_base(), nxkit_metal::GUEST_VA_BASE);
    let addr = map.gpu_base() + 0x100;
    let f = frame(addr);
    let p = plan(&f);
    let ords = crate::draws::ordinals(&f);
    let pool = Arc::new(VisPool::default());
    let q = prepare(&ctx, &pool, &f, &p, &detail(), &ords, None).unwrap();
    let (s0, s1) = (q.draw_slot(0).unwrap(), q.draw_slot(1).unwrap());
    assert_ne!(s0, s1);
    assert_eq!(q.draw_slot(2), None);
    let render = p
        .passes
        .iter()
        .position(|p| p.kind == PassKind::Render)
        .unwrap();
    let vis = q.pass(render).unwrap();
    let (o0, o1) = (vis.offset(s0).unwrap(), vis.offset(s1).unwrap());
    assert_ne!(o0, o1);
    vis.buffer.write(o0, &40u64.to_le_bytes()).unwrap();
    vis.buffer.write(o1, &2u64.to_le_bytes()).unwrap();
    // Draw 1 is not encoded: the second samples report does not land. The first sums no slot, the vertices of draw 0
    // are known from its arguments.
    q.encoded(s0);
    let st = q.write(&guest, 7);
    assert_eq!((st.written, st.samples, st.cpu, st.uncounted), (2, 1, 1, 1));
    assert_eq!(counter(&guest, 0x120), (3, 7));
    let landed = f.counters.landed.as_ref().unwrap();
    assert_eq!(landed.get(addr + 16), None);
    q.encoded(s1);
    let st = q.write(&guest, 9);
    assert_eq!((st.written, st.samples), (3, 2));
    assert_eq!(counter(&guest, 0x100), (0, 9));
    assert_eq!(counter(&guest, 0x110), (42, 9));
    assert_eq!(landed.get(addr + 16).map(|(v, _)| v), Some(42));
}

#[test]
fn a_report_outside_guest_memory_is_not_written() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let guest = space(&ctx, 0x1000);
    let map = guest.address_map();
    let end = map.gpu_base() + map.len();
    assert!(write_report(&guest, &map, end - 8, 1, 2).is_none());
    assert!(write_report(&guest, &map, map.gpu_base() - 16, 1, 2).is_none());
    assert!(write_report(&guest, &map, end - 16, 1, 2).is_some());
    // The counter lands at the offset of the virtual address, and the real address of the buffer is outside.
    assert_eq!(counter(&guest, map.len() - 16), (1, 2));
    let real = guest_real(&ctx);
    assert!(write_report(&guest, &map, real, 1, 2).is_none());
}

/// The real address of a guest buffer. No virtual address may be equal to it, or near.
fn guest_real(ctx: &Context) -> u64 {
    nxkit_metal::GuestMemory::allocate(ctx, 0x1000)
        .unwrap()
        .real_base()
}
