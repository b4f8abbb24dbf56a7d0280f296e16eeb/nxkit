//! The byte sinks of the argument buffers (`Sink`): the per-batch scratch buffer, the arena and the draw to draw memo.

use super::*;

/// A place that takes bytes and returns the GPU address of the copy.
pub(crate) trait Sink {
    fn put(&mut self, data: &[u8]) -> Result<u64, Skip>;

    /// Puts bytes that many draws share. A sink may put them once and return the same address again.
    fn put_shared(&mut self, data: &Arc<[u8]>) -> Result<u64, Skip> {
        self.put(data)
    }

    /// The memo of the argument buffers that this sink's draws share, when it keeps one.
    fn memo(&mut self) -> Option<&mut ArgsMemo> {
        None
    }
}

/// The argument buffers that the previous draws of one render pass wrote. A draw that binds what the draw before it
/// bound per stage reuses that buffer instead of writing a new one. The addresses point into the scratch buffer of
/// the batch, so a memo must not outlive the pass.
#[derive(Default)]
pub(crate) struct ArgsMemo {
    stages: [StageMemo; 2],
    /// The layout block of the last pulled draw and its address (`crate::pull`).
    layout: Option<(crate::pull::Block, u64)>,
}

/// The last argument buffer of one stage, with the words that describe what it was built from.
#[derive(Default)]
struct StageMemo {
    last: Vec<u64>,
    next: Vec<u64>,
    addr: u64,
    valid: bool,
}

impl ArgsMemo {
    /// The address of the argument buffer for `inp`, when the previous draw of the stage built it from the same
    /// inputs. A miss prepares [`ArgsMemo::store`].
    pub(super) fn lookup(
        &mut self,
        p: &StageProgram,
        inp: &StageInputs<'_>,
        pass_scale: f32,
    ) -> Option<u64> {
        let m = match inp.stage {
            Stage::Vertex => &mut self.stages[0],
            Stage::Fragment => &mut self.stages[1],
            Stage::Compute | Stage::Task | Stage::Mesh => return None,
        };
        m.next.clear();
        m.next.push(p as *const StageProgram as u64);
        m.next.push(u64::from(inp.base_instance));
        m.next.push(u64::from(pass_scale.to_bits()));
        m.next.push(u64::from(inp.blendz));
        // The push constants are shared until the game pushes again, so their address names their content.
        m.next.push(inp.push.map_or(0, |b| b.as_ptr() as u64));
        let mark = m.next.len();
        m.next.push(0);
        for u in inp.ubos.iter().filter(|u| u.stage == inp.stage) {
            let snapshot = u.snapshot.as_ref().map_or(0, |b| b.as_ptr() as u64);
            m.next
                .extend([u64::from(u.slot), snapshot, u.address, u64::from(u.size)]);
        }
        let ubos = m.next.len() - mark - 1;
        m.next[mark] = ubos as u64;
        for s in inp.sbufs.iter().filter(|s| s.stage == inp.stage) {
            let snapshot = s.snapshot.as_ref().map_or(0, |b| b.as_ptr() as u64);
            m.next.extend([u64::from(s.slot), s.address, snapshot]);
        }
        m.next.push(u64::MAX);
        for t in inp.textures.iter().filter(|t| t.stage == inp.stage) {
            m.next.extend([u64::from(t.slot), t.handle]);
        }
        m.next.push(u64::MAX);
        for t in inp.images.iter().filter(|t| t.stage == inp.stage) {
            m.next.extend([u64::from(t.slot), t.handle]);
        }
        if m.valid && m.next == m.last {
            return Some(m.addr);
        }
        std::mem::swap(&mut m.last, &mut m.next);
        m.valid = false;
        None
    }

    /// The address of `block` when the last pulled draw put the same block.
    pub(crate) fn layout(&self, block: &crate::pull::Block) -> Option<u64> {
        self.layout
            .as_ref()
            .filter(|(b, _)| b == block)
            .map(|(_, a)| *a)
    }

    /// Records the address of the layout block that a pulled draw put.
    pub(crate) fn store_layout(&mut self, block: &crate::pull::Block, addr: u64) {
        self.layout = Some((*block, addr));
    }

    /// Records the buffer that was built after a miss of [`ArgsMemo::lookup`].
    pub(super) fn store(&mut self, stage: Stage, addr: u64) {
        let m = match stage {
            Stage::Vertex => &mut self.stages[0],
            Stage::Fragment => &mut self.stages[1],
            Stage::Compute | Stage::Task | Stage::Mesh => return,
        };
        m.addr = addr;
        m.valid = true;
    }
}

/// The per-batch scratch buffer as a sink.
pub(crate) struct ScratchSink<'a> {
    scratch: &'a Scratch,
    uploaded: &'a Mutex<crate::draws::FastMap<usize, u64>>,
    preloaded: Option<&'a crate::draws::FastMap<usize, u64>>,
    memo: Option<&'a mut ArgsMemo>,
}

impl<'a> ScratchSink<'a> {
    /// A sink that writes into `scratch`. `uploaded` remembers the shared snapshots of the batch. A `memo` lets
    /// draws of one pass share argument buffers.
    pub(crate) fn new(
        scratch: &'a Scratch,
        uploaded: &'a Mutex<crate::draws::FastMap<usize, u64>>,
        memo: Option<&'a mut ArgsMemo>,
    ) -> Self {
        Self {
            scratch,
            uploaded,
            preloaded: None,
            memo,
        }
    }

    /// Looks shared snapshots up in `preloaded` first, without a lock.
    pub(crate) fn with_preloaded(
        mut self,
        preloaded: &'a crate::draws::FastMap<usize, u64>,
    ) -> Self {
        self.preloaded = Some(preloaded);
        self
    }
}

impl Sink for ScratchSink<'_> {
    fn put_shared(&mut self, data: &Arc<[u8]>) -> Result<u64, Skip> {
        let key = data.as_ptr() as usize;
        if let Some(a) = self.preloaded.and_then(|m| m.get(&key).copied()) {
            return Ok(a);
        }
        if let Some(a) = self.uploaded.lock().ok().and_then(|m| m.get(&key).copied()) {
            return Ok(a);
        }
        let a = self.put(data)?;
        if let Ok(mut m) = self.uploaded.lock() {
            m.insert(key, a);
        }
        Ok(a)
    }

    fn put(&mut self, data: &[u8]) -> Result<u64, Skip> {
        self.scratch
            .push(data)
            .map_err(|e| crate::buf_skip(format_args!("scratch push {} bytes: {e:?}", data.len())))
    }

    fn memo(&mut self) -> Option<&mut ArgsMemo> {
        self.memo.as_deref_mut()
    }
}

/// A persistent buffer that fills from the front. An ICB keeps its argument buffers here.
pub(crate) struct Arena {
    pub(crate) buffer: Arc<DataBuffer>,
    cursor: u64,
}

impl Arena {
    pub(crate) fn new(ctx: &Context, len: u64) -> Result<Self, Skip> {
        Ok(Self {
            buffer: Arc::new(DataBuffer::new(ctx, len.max(256)).map_err(|_| Skip::Buffer)?),
            cursor: 0,
        })
    }

    /// The offset of a GPU address inside the buffer.
    pub(crate) fn offset_of(&self, addr: u64) -> u64 {
        addr - self.buffer.gpu_address()
    }
}

impl Sink for Arena {
    fn put(&mut self, data: &[u8]) -> Result<u64, Skip> {
        let at = self.cursor.next_multiple_of(16);
        self.buffer.write(at, data).map_err(|_| Skip::Buffer)?;
        self.cursor = at + data.len() as u64;
        Ok(self.buffer.gpu_address() + at)
    }
}
