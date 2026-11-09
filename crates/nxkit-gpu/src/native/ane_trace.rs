//! `NXKIT_DIAG=ndvetrace`: a one-shot report on why the decode-time Neural Engine path finds no picture for a dispatch.
//!
//! The path looks for the picture the video service wrote (`ReadDecoderOutputImageNv12`, tight NV12 at a guest address)
//! in the planes the game's NDVE dispatch binds. If the game puts the picture into those planes by a way the feeder
//! does not follow, nothing matches. At the first dispatch of the run that reaches the Neural Engine path
//! (on unless `NXKIT_NDVE_ANE=0`), and again 60 dispatches later, the report prints:
//!
//! 1. the descriptors of the luma and chroma textures as the game set them, the memory pool they lie in, and where the
//!    decoder wrote its latest picture: the same pool or not, the same VM object at the same offset (two views of the
//!    same pages) or not, and whether the first 4 KiB of both are equal;
//! 2. every command of the current frame and the one before that writes the textures or their memory: copies of any
//!    kind, compute dispatches and draws that bind it as an image or a storage buffer, render passes with it as an
//!    attachment, and what the dirty tracking knows of CPU writes (the write epoch and the residency of the record);
//! 3. the first 64 bytes of each plane as the texture's layout reads them, the same bytes of the decoder's latest
//!    picture, and the fingerprint of each ([`super::print`]) with the decoded picture it equals, if any.
//!
//! The state is a copy of the last two frames' commands, kept only while this category is on and until the second
//! report is out. Nothing here runs without it.

use std::sync::atomic::Ordering;

use nxkit_metal::vm_region;
use nxkit_nvn::resource::TextureRecord;
use nxkit_nvn::tic::{Tic, TicLayout};

use super::feed::feed;
use super::print::{self, Layout, Print};
use super::trace::Copies;
use writers::{Snap, Target, meets, nearest_copies, writers};

mod writers;
use super::*;

/// The dispatches between the first report and the second.
const SECOND: u32 = 60;

/// The bytes that the two views of a plane are compared over.
const COMPARE: usize = 4096;

/// The bytes of a plane that the report prints.
const HEAD: u64 = 64;

#[derive(Default)]
struct State {
    cur: Option<Snap>,
    prev: Option<Snap>,
    /// The reports printed.
    reports: u32,
}

static STATE: Mutex<State> = Mutex::new(State {
    cur: None,
    prev: None,
    reports: 0,
});
static DISPATCHES: AtomicU32 = AtomicU32::new(0);

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn on() -> bool {
    crate::diag("ndvetrace")
}

/// Takes the frame's copies for the chain ([`Copies`]) and, with the category on, a snapshot of the frame.
pub(super) fn frame(copies: &mut Copies, cx: &PrepareCx<'_>) {
    copies.ingest(&cx.detail.copies);
    if !on() || state().reports >= 2 {
        return;
    }
    let snap = Snap::of(cx);
    let mut st = state();
    let old = st.cur.replace(snap);
    st.prev = old;
}

/// A plane of the dispatch as the game bound it.
struct Plane {
    what: &'static str,
    handle: u64,
    id: TexId,
    tic: Tic,
    metal: String,
    host: Option<u64>,
}

impl Plane {
    fn size(&self) -> (u32, u32) {
        (self.tic.width, self.tic.height)
    }

    fn layout(&self) -> Option<Layout> {
        match self.tic.layout {
            TicLayout::Pitch { stride } => Some(Layout::Pitch(u64::from(stride))),
            TicLayout::BlockLinear { tile_h, .. } => Some(Layout::Block(u32::from(tile_h))),
            TicLayout::Buffer => None,
        }
    }
}

fn plane(
    what: &'static str,
    res: GameResource,
    specs: &[ResourceSpec],
    p: &Prepared,
    cx: &PrepareCx<'_>,
) -> Option<Plane> {
    let spec = specs.iter().find(|s| s.resource == res)?;
    let (dispatch, slot, image) = match spec.place {
        Place::Texture { dispatch, slot } => (dispatch, slot, false),
        Place::Image { dispatch, slot } => (dispatch, slot, true),
        Place::Push { .. } => return None,
    };
    let dd = cx.detail.dispatches.get(p.first as usize + dispatch)?;
    let bind = bind_of(if image { &dd.images } else { &dd.textures }, slot)?;
    let (id, r, tic) = cx.resolve(bind.handle)?;
    let host = cx
        .guest_offset(tic.address.get(), 1)
        .map(|o| cx.guest.host_base() + o);
    Some(Plane {
        what,
        handle: bind.handle,
        id,
        tic: *tic,
        metal: format!(
            "{:?}{}",
            r.desc.format,
            if r.scaled { " scaled" } else { "" }
        ),
        host,
    })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The guest memory at a GPU address.
fn read(cx: &PrepareCx<'_>, gpu: u64, n: usize) -> Option<Vec<u8>> {
    let mut b = vec![0u8; n];
    cx.guest
        .read(cx.guest_offset(gpu, n as u64)?, &mut b)
        .ok()?;
    Some(b)
}

/// The GPU address of a host address of the guest memory.
fn gpu_of(cx: &PrepareCx<'_>, host: u64) -> Option<u64> {
    cx.guest.gpu_address(host, 1).ok()
}

fn pool_line(cx: &PrepareCx<'_>, gpu: u64, end: u64) -> String {
    let map = cx.guest.address_map();
    let Some((s, e, flags)) = cx.pools.0.pool_of(gpu) else {
        return "in no memory pool the game made".into();
    };
    let cpu = |a: u64| map.host_base() + (a - map.gpu_base());
    format!(
        "pool gpu {s:#x}..{e:#x} guest {:#x}..{:#x} flags {flags:#x} (cpu no access {}), whole range inside {}",
        cpu(s),
        cpu(e),
        flags & 1 != 0,
        end <= e
    )
}

fn describe(p: &Plane, cx: &PrepareCx<'_>, rec: Option<&TextureRecord>, out: &mut Vec<String>) {
    let t = &p.tic;
    let host = p
        .host
        .map_or("outside guest memory".to_string(), |h| format!("{h:#x}"));
    out.push(format!(
        "{}: handle {:#x} texture {} gpu {:#x} host/guest {host} format {} (metal {}) {}x{}x{} levels {} samples {} target {:?} layout {:?}",
        p.what, p.handle, p.id.0, t.address.get(), t.format, p.metal, t.width, t.height, t.depth, t.levels, t.samples, t.target, t.layout
    ));
    if let Some(r) = rec {
        out.push(format!(
            "{}: record size {:#x} align {} tile_h {} linear {} desc.stride {} write_epoch {} residency {:?}",
            p.what, r.layout.size, r.layout.align, r.layout.tile_h, r.layout.linear, r.desc.stride, r.write_epoch, r.residency
        ));
        let end = r.end();
        out.push(format!(
            "{}: {}",
            p.what,
            pool_line(cx, t.address.get(), end)
        ));
    }
}

/// The decoder's side of a plane against the texture's: pool, VM object and bytes.
fn compare(what: &str, tex: u64, dec: u64, cx: &PrepareCx<'_>, out: &mut Vec<String>) {
    out.push(format!(
        "{what}: texture host {tex:#x}, decoder host {dec:#x}, distance {:+#x}",
        i128::from(dec) - i128::from(tex)
    ));
    let (a, b) = (vm_region(tex), vm_region(dec));
    for (n, h, r) in [("texture", tex, a), ("decoder", dec, b)] {
        match r {
            Some(r) => out.push(format!(
                "{what}: {n} VM region {:#x}+{:#x} prot {} share mode {} object {:#x} offset in object {:#x} users {} tag {}",
                r.start, r.size, r.protection, r.share_mode, r.object, r.offset_of(h), r.ref_count, r.tag
            )),
            None => out.push(format!("{what}: {n} address is not in a VM region")),
        }
    }
    let alias = a.zip(b).map(|(x, y)| x.aliases(tex, &y, dec));
    out.push(format!(
        "{what}: the two addresses are the same bytes of one VM object: {alias:?}"
    ));
    let bytes = |h: u64| gpu_of(cx, h).and_then(|g| read(cx, g, COMPARE));
    match (bytes(tex), bytes(dec)) {
        (Some(x), Some(y)) => {
            let equal = x.iter().zip(&y).filter(|(p, q)| p == q).count();
            let first = x.iter().zip(&y).position(|(p, q)| p != q);
            let nz = |v: &[u8]| v.iter().filter(|b| **b != 0).count();
            out.push(format!(
                "{what}: first {COMPARE} bytes: {equal} equal, first difference at {first:?}, non-zero bytes {} texture / {} decoder",
                nz(&x),
                nz(&y)
            ));
        }
        _ => out.push(format!(
            "{what}: one of the 4 KiB ranges is outside guest memory"
        )),
    }
}

/// The fingerprint of a plane in memory at `gpu`.
fn print_at(cx: &PrepareCx<'_>, gpu: u64, size: (u32, u32), layout: Layout) -> Option<Print> {
    let byte = |o: u64| {
        let b = read(cx, gpu.checked_add(o)?, 1)?;
        Some(b[0])
    };
    print::of_memory(size, layout, byte)
}

fn print_line(what: &str, p: Option<Print>, prints: &[(Print, super::slots::Pic)]) -> String {
    let m = p.and_then(|p| prints.iter().rev().find(|(q, _)| *q == p));
    let tail = match (p, m) {
        (None, _) => "no fingerprint (memory too short or empty)".to_string(),
        (Some(_), Some((_, pic))) => format!("equals the decoded picture {pic}"),
        (Some(_), None) => format!("equals none of the {} decoded fingerprints", prints.len()),
    };
    format!("{what}: fingerprint {p:?} {tail}")
}

/// The report of both planes.
fn report(
    specs: &[ResourceSpec],
    pic: PictureSpec,
    p: &Prepared,
    cx: &PrepareCx<'_>,
    n: u32,
) -> Vec<String> {
    let mut out = vec![format!(
        "DIAG ndvetrace report {n} (dispatch {}): the dispatch's planes",
        DISPATCHES.load(Ordering::Relaxed)
    )];
    let planes = [("luma", pic.luma), ("chroma", pic.chroma)];
    let planes: Vec<Plane> = planes
        .iter()
        .filter_map(|(w, r)| plane(w, *r, specs, p, cx))
        .collect();
    let (binds, prints) = feed().trace_view();
    out.push(format!(
        "decoder: {} reads of pictures known, decoded fingerprints {}",
        binds.len(),
        prints.len()
    ));
    for (l, c, pic) in binds.iter().rev().take(3) {
        let (gl, gc) = (gpu_of(cx, *l), gpu_of(cx, *c));
        out.push(format!("decoder: picture {pic} luma host {l:#x} (gpu {gl:x?}) chroma host {c:#x} (gpu {gc:x?}) {}", pool_line(cx, gl.unwrap_or(0), 0)));
    }
    let last = binds.last().copied();
    let st = state();
    for pl in &planes {
        let rec = st
            .cur
            .as_ref()
            .and_then(|s| s.recs.get(&pl.id))
            .or_else(|| cx.view.record(pl.id).ok());
        describe(pl, cx, rec, &mut out);
        if let (Some(tex), Some((l, c, _))) = (pl.host, last) {
            compare(
                pl.what,
                tex,
                if pl.what == "luma" { l } else { c },
                cx,
                &mut out,
            );
        }
    }
    writes(&planes, &st, cx, &mut out);
    drop(st);
    bytes_of(&planes, last, &prints, cx, &mut out);
    out
}

/// The writers of each plane in the current and in the previous frame, and what the dirty tracking knows.
fn writes(planes: &[Plane], st: &State, cx: &PrepareCx<'_>, out: &mut Vec<String>) {
    for pl in planes {
        let Some(r) = cx.view.record(pl.id).ok() else {
            continue;
        };
        let t = Target {
            id: pl.id,
            lo: r.address.get(),
            hi: r.end(),
        };
        let before = st.prev.as_ref().and_then(|s| s.recs.get(&t.id));
        out.push(format!(
            "{}: dirty tracking: write_epoch now {} (previous frame {:?}), residency {:?} (previous {:?})",
            pl.what, r.write_epoch, before.map(|b| b.write_epoch), r.residency, before.map(|b| b.residency)
        ));
        for (label, snap) in [("this frame", &st.cur), ("previous frame", &st.prev)] {
            let Some(s) = snap else {
                out.push(format!("{}: {label}: no snapshot", pl.what));
                continue;
            };
            let w = writers(s, &t);
            let n = w.lines.len() + w.more;
            out.push(format!(
                "{}: {label}: {n} writers of {:#x}..{:#x} ({} commands); {}",
                pl.what,
                t.lo,
                t.hi,
                s.frame.cmds.len(),
                nearest_copies(s, &t)
            ));
            out.extend(
                w.lines
                    .into_iter()
                    .map(|l| format!("{}: {label}: {l}", pl.what)),
            );
            if w.more > 0 {
                out.push(format!("{}: {label}: and {} more", pl.what, w.more));
            }
        }
        for (a, b) in cx.pools.1.iter().filter(|r| meets(**r, &t)) {
            out.push(format!(
                "{}: shaders wrote through raw pointers into the pool {a:#x}..{b:#x}",
                pl.what
            ));
        }
    }
}

/// The first bytes of each plane in the texture's layout and of the decoder's latest picture, with fingerprints.
fn bytes_of(
    planes: &[Plane],
    last: Option<super::feed::ReadOf>,
    prints: &[(Print, super::slots::Pic)],
    cx: &PrepareCx<'_>,
    out: &mut Vec<String>,
) {
    for pl in planes {
        let gpu = pl.tic.address.get();
        let raw = read(cx, gpu, HEAD as usize).map(|b| hex(&b));
        out.push(format!(
            "{}: texture first {HEAD} bytes in memory {raw:?}",
            pl.what
        ));
        let Some(layout) = pl.layout() else { continue };
        let size = pl.size();
        let row: Option<Vec<u8>> = (0..HEAD as u32)
            .map(|x| read(cx, gpu.checked_add(layout.offset((x, 0), size))?, 1).map(|b| b[0]))
            .collect();
        out.push(format!(
            "{}: texture row 0, texels 0..{HEAD} through the layout {:?}: {:?}",
            pl.what,
            pl.tic.layout,
            row.map(|b| hex(&b))
        ));
        if pl.what != "luma" {
            continue;
        }
        out.push(print_line(
            "luma texture",
            print_at(cx, gpu, size, layout),
            prints,
        ));
        if let Some((l, _, pic)) = last
            && let Some(g) = gpu_of(cx, l)
        {
            let dec = read(cx, g, HEAD as usize).map(|b| hex(&b));
            out.push(format!(
                "luma decoder picture {pic}: first {HEAD} bytes at {l:#x} (tight, row 0) {dec:?}"
            ));
            out.push(print_line(
                "luma decoder picture",
                print_at(cx, g, size, Layout::Pitch(u64::from(size.0))),
                prints,
            ));
        }
    }
    for (p, pic) in prints.iter().rev().take(3) {
        out.push(format!("decoded picture {pic}: fingerprint {p:?}"));
    }
}

/// Called for every dispatch that reaches the Neural Engine path: prints the report for the first and for the
/// [`SECOND`]th.
pub(super) fn dispatch(specs: &[ResourceSpec], pic: PictureSpec, p: &Prepared, cx: &PrepareCx<'_>) {
    if !on() {
        return;
    }
    let n = DISPATCHES.fetch_add(1, Ordering::Relaxed);
    if n != 0 && n != SECOND {
        return;
    }
    let reports = state().reports;
    for l in report(specs, pic, p, cx, reports + 1) {
        eprintln!("{l}");
    }
    state().reports += 1;
}
