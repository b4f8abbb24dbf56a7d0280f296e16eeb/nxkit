//! The tests of the template cache.

mod keys;
mod origin;
mod walks;

use super::*;
use crate::detail::FrameDetail;
use nxkit_metal::AddressMap;
use nxkit_nvn::backend::{ShaderKey, StageKey};
use nxkit_nvn::cmdbuf::CommandHandle;
use nxkit_nvn::ext::{DrawKind, ExtCode, Setting};
use nxkit_nvn::mem::VecMem;
use nxkit_nvn::record::Record;

const HOST: u64 = 0x7000_0000;
const GPU: u64 = 0x15_0400_8000;
const SIZE: u64 = 1 << 20;
/// The first free address for recordings.
const CODE: u64 = HOST + 0x8_0000;
const PROG: u64 = GPU + 0x9000;
const UBO_AT: u64 = GPU + 0x1000;
const SRC: u64 = HOST + 0x3000;

fn ext(code: ExtCode, arg: u16, words: &[u32]) -> Record {
    Record::Ext(words.iter().fold(Ext::new(code, arg), |x, w| x.u32(*w)))
}

fn halves(v: u64) -> [u32; 2] {
    [v as u32, (v >> 32) as u32]
}

fn program(addr: u64) -> Record {
    let [lo, hi] = halves(addr);
    ext(ExtCode::Program, 0, &[lo, hi, 0b11])
}

fn viewport(width: f32) -> Record {
    ext(
        ExtCode::Viewport,
        0,
        &[0, 0, width.to_bits(), 90f32.to_bits()],
    )
}

fn ubo(slot: u16, addr: u64, size: u32) -> Record {
    let [lo, hi] = halves(addr);
    ext(ExtCode::UniformBuffer, slot, &[lo, hi, size, 0])
}

fn vertex_buffer(stream: u16, addr: u64, size: u32) -> Record {
    let [lo, hi] = halves(addr);
    ext(ExtCode::VertexBuffer, stream, &[lo, hi, size, 0])
}

fn attribute(location: u32, format: u32, stream: u32) -> Record {
    ext(
        ExtCode::VertexState,
        0,
        &[location, 1, format | stream << 27],
    )
}

fn stream(index: u32, stride: u32) -> Record {
    ext(ExtCode::VertexState, 1, &[index, 1, stride, 0])
}

fn draw() -> Record {
    ext(ExtCode::Draw, DrawKind::Arrays as u16, &[4, 0, 3])
}

fn dispatch() -> Record {
    ext(ExtCode::Dispatch, 0, &[2, 3, 4])
}

fn clear() -> Record {
    ext(ExtCode::ClearColor, 0, &[0xf, 1f32.to_bits(), 0, 0, 0])
}

fn update(buffer: u64, size: u32, offset: u32, value: u32) -> Record {
    let [lo, hi] = halves(buffer);
    ext(
        ExtCode::UpdateUniform,
        4,
        &[lo, hi, size, 0, offset, 0, value],
    )
}

fn blend(target: u32, factor: u32) -> Record {
    let v = u64::from(target) | u64::from(factor) << 3 | u64::from(factor) << 10;
    ext(ExtCode::State, 0, &[v as u32, (v >> 32) as u32])
}

fn stencil_ref(face: u32, value: u32) -> Record {
    ext(ExtCode::Setting, Setting::StencilRef as u16, &[face, value])
}

fn deferred(first: u8, count: u8, src: u64) -> Record {
    Record::BindTexturesDeferred {
        first,
        count,
        src,
        tex_base: 0,
        samp_base: 0,
    }
}

/// A model recording: its own buffers and vertex layout, then `draws` draws.
fn model(buffer: u64, draws: usize) -> Vec<Record> {
    let mut r = vec![
        ubo(0, buffer, 64),
        vertex_buffer(0, GPU + 0x2000, 256),
        attribute(0, 0x0a, 0),
        stream(0, 16),
    ];
    r.extend((0..draws).map(|_| draw()));
    r
}

/// A walker over a block of guest memory.
struct Rig {
    mem: VecMem,
    w: Walker,
    at: u64,
}

impl Rig {
    fn new() -> Self {
        let map = AddressMap::new(HOST, GPU, SIZE).unwrap();
        let mut w = Walker::new(Default::default(), map);
        w.program_set(PROG, &keys(1, 2));
        Self {
            mem: VecMem::new(HOST, SIZE as usize),
            w,
            at: CODE,
        }
    }

    fn put_at(&mut self, at: u64, recs: &[Record]) -> u64 {
        let mut p = at;
        for rec in recs.iter().chain([&Record::End]) {
            let bytes = rec.encode().unwrap();
            self.mem.write(p, bytes.as_bytes()).unwrap();
            p += bytes.len() as u64;
        }
        at
    }

    fn put(&mut self, recs: &[Record]) -> u64 {
        let at = self.at;
        self.put_at(at, recs);
        self.at += 0x1000;
        at
    }

    fn frame(&mut self, handles: &[u64]) -> (FrameDetail, super::super::WalkCounters) {
        let h: Vec<CommandHandle> = handles.iter().map(|h| CommandHandle(*h)).collect();
        self.w.submit(&mut self.mem, 1, &h);
        let c = self.w.take_counters();
        (self.w.take(), c)
    }

    fn bytes(&self, at: u64, len: usize) -> Vec<u8> {
        let mut b = vec![0; len];
        self.mem.read(at, &mut b).unwrap();
        b
    }
}

/// Two walkers over the same guest memory: `cached` plays templates, `plain` walks every record.
struct Pair {
    cached: Rig,
    plain: Rig,
}

impl Pair {
    /// With `verify`, the cached walker also compares itself with an uncached walk, and returns the uncached result.
    fn new(verify: bool) -> Self {
        let mut cached = Rig::new();
        cached.w.verify = verify;
        let mut plain = Rig::new();
        plain.w.cache_on = false;
        plain.w.verify = false;
        Self { cached, plain }
    }

    fn put(&mut self, recs: &[Record]) -> u64 {
        let h = self.cached.put(recs);
        assert_eq!(h, self.plain.put(recs));
        h
    }

    fn put_at(&mut self, at: u64, recs: &[Record]) {
        self.cached.put_at(at, recs);
        self.plain.put_at(at, recs);
    }

    fn write(&mut self, at: u64, bytes: &[u8]) {
        self.cached.mem.write(at, bytes).unwrap();
        self.plain.mem.write(at, bytes).unwrap();
    }

    /// The game wrote the command memory `[at, at + len)` and told the backend.
    fn rewritten(&mut self, at: u64, len: u64) {
        self.cached.w.invalidations().push(at, at + len);
    }

    /// Walks the frame with both and compares what they produced, the memory they wrote and the counters.
    fn frame(&mut self, handles: &[u64]) -> (FrameDetail, super::super::WalkCounters) {
        let (a, counters) = self.cached.frame(handles);
        let (b, _) = self.plain.frame(handles);
        assert_eq!(a.draws.len(), b.draws.len());
        for (i, (x, y)) in a.draws.iter().zip(&b.draws).enumerate() {
            assert!(x == y, "draw {i}: {}", differences(x, y));
        }
        assert_eq!(a.dispatches.len(), b.dispatches.len());
        for (i, (x, y)) in a.dispatches.iter().zip(&b.dispatches).enumerate() {
            assert!(x == y, "dispatch {i}: {x:?} against {y:?}");
        }
        assert_eq!(a.clears, b.clears);
        assert_eq!(a.copies, b.copies);
        assert_eq!(a.reports, b.reports);
        assert_eq!(
            self.cached.bytes(HOST, 0x4000),
            self.plain.bytes(HOST, 0x4000)
        );
        (a, counters)
    }
}

/// The names of the fields in which two draws differ.
fn differences(a: &DrawDetail, b: &DrawDetail) -> String {
    let mut out = Vec::new();
    macro_rules! check {
        ($($f:ident),*) => { $(if a.$f != b.$f { out.push(format!("{}: {:?} against {:?}", stringify!($f), a.$f, b.$f)); })* };
    }
    check!(
        program,
        blend,
        write_masks,
        depth_stencil,
        raster,
        topology,
        attributes,
        streams,
        args,
        ubos,
        sbufs,
        dynamic
    );
    if a.textures != b.textures {
        let slots: Vec<String> = a
            .textures
            .iter()
            .zip(&b.textures)
            .filter(|(x, y)| x != y)
            .map(|(x, y)| format!("slot {} {} against {}", x.slot, x.handle, y.handle))
            .collect();
        out.push(format!(
            "textures ({} against {}): {}",
            a.textures.len(),
            b.textures.len(),
            slots.join(", ")
        ));
    }
    out.join("; ")
}

fn keys(vertex: u8, fragment: u8) -> [StageKey; 2] {
    let stage = |index: u8, k: u8| StageKey {
        index,
        key: Some(ShaderKey([k; 8])),
        code: 0,
        code_len: 0,
    };
    [stage(0, vertex), stage(1, fragment)]
}

fn for_both(scenario: impl Fn(&mut Pair)) {
    for verify in [false, true] {
        scenario(&mut Pair::new(verify));
    }
}
