//! The lookup of a template, and the walk that builds one.

use super::*;

impl Walker {
    /// The index of the template of `handle` that fits `st` at `depth`.
    pub(super) fn find(&mut self, st: &State, handle: u64, depth: u32) -> Option<usize> {
        let epoch = self.programs.epoch();
        let e = self.cache.entries.get_mut(&handle)?;
        let mut stale = 0u64;
        let mut found = None;
        // The digest of `st` for the fields of template `shape`. The templates of a handle mostly read the same fields,
        // so it is computed once per distinct field set.
        let mut shape: Option<(usize, u64)> = None;
        for i in (0..e.list.len()).rev() {
            let d = match shape {
                Some((j, d)) if e.list[j].key.segs == e.list[i].key.segs => d,
                _ => {
                    let d = e.list[i].key.digest_of(st, self.track.prov);
                    shape = Some((i, d));
                    d
                }
            };
            let t = &mut e.list[i];
            if t.key.digest != d || depth + t.height > MAX_CALL_DEPTH {
                continue;
            }
            if t.epoch != epoch {
                if t.programs
                    .iter()
                    .all(|(p, g)| self.programs.generation(*p) == *g)
                {
                    t.epoch = epoch;
                } else {
                    stale |= 1 << i;
                    continue;
                }
            }
            if t.key.matches(st, self.track.prov) {
                found = Some(i);
                break;
            }
        }
        if found.is_none()
            && stale == 0
            && crate::diag("walkmiss")
            && let Some(t) = e.list.last()
        {
            walkmiss(t.key.why_not(st, self.track.prov), e.list.len());
        }
        if stale != 0 {
            let mut i = e.list.len();
            while i > 0 {
                i -= 1;
                if stale >> i & 1 != 0 {
                    e.list.remove(i);
                    found = found.map(|f| if f > i { f - 1 } else { f });
                }
            }
            if e.list.is_empty() {
                self.cache.drop_handle(handle);
                return None;
            }
        }
        found
    }

    /// Walks the recording and keeps a template of it.
    /// A cold handle (`keep` false) is tracked the same way, but its template is not made.
    pub(super) fn build<M: GuestMem>(
        &mut self,
        mem: &mut M,
        st: &mut State,
        handle: u64,
        depth: u32,
        keep: bool,
    ) {
        let mut entry = self
            .track
            .pool
            .pop()
            .unwrap_or_else(|| Box::new(State::default()));
        (*entry).clone_from(st);
        let open = Open {
            needs: Bits::NONE,
            written: Bits::NONE,
            entry,
            prov: self.track.prov,
            marks: self.marks(),
            dep0: self.track.dep_list.len(),
            patch0: self.track.patches.len(),
            upd0: self.track.upd_log.len(),
            programs: Vec::new(),
            spans: Vec::new(),
            height: 0,
            truncated: false,
        };
        self.track.open.push(open);
        self.walk_records(mem, st, handle, depth);
        let Some(f) = self.track.open.pop() else {
            return;
        };
        self.counters.walk_misses += 1;
        if let Some(p) = self.track.open.last_mut() {
            p.absorb(
                &f.needs,
                &f.written,
                f.programs.iter().copied(),
                &f.spans,
                f.height,
                f.truncated,
            );
        }
        if f.truncated || !keep {
            self.track.pool.push(f.entry);
            return;
        }
        let t = self.template(&f, st);
        self.track.pool.push(f.entry);
        self.cache.insert(handle, t);
    }

    fn template(&self, f: &Open, st: &State) -> Box<Template> {
        let key = Key::of(&f.needs, &f.entry, f.prov);
        let m = f.marks;
        let (draws, dispatches) = (m.draws as u32, m.dispatches as u32);
        let draws_v = self.out.draws[m.draws..].to_vec();
        let dispatches_v = self.out.dispatches[m.dispatches..].to_vec();
        let updates: Vec<Upd> = self.track.upd_log[f.upd0..]
            .iter()
            .map(|u| Upd {
                ext: u.ext,
                draws: u.draws - draws,
                dispatches: u.dispatches - dispatches,
            })
            .collect();
        let rebase = |p: &Patch| Patch {
            idx: p.idx - if p.dispatch { dispatches } else { draws },
            ..*p
        };
        let patches = self.track.patches[f.patch0..]
            .iter()
            .filter(|p| p.dep as usize >= f.dep0)
            .map(|p| Patch {
                dep: p.dep - f.dep0 as u32,
                ..rebase(p)
            })
            .collect();
        let inherited = self.track.patches[f.patch0..]
            .iter()
            .filter(|p| (p.dep as usize) < f.dep0)
            .map(rebase)
            .collect();
        let mut delta = Vec::new();
        let mut groups = 0u32;
        for bit in f.written.iter() {
            let d = match bit {
                P..VB => Delta::Program((bit - P) as u8, st.program[bit - P]),
                VB..UBO => Delta::VertexBuffer((bit - VB) as u8, st.vertex_buffer[bit - VB]),
                UBO..SBO => {
                    let i = bit - UBO;
                    Delta::Ubo(i as u8, st.ubo[i / 16][i % 16])
                }
                SBO..ATTR => {
                    let i = bit - SBO;
                    Delta::Sbo(i as u8, st.sbuf[i / 16][i % 16])
                }
                ATTR..STREAM => Delta::Attribute((bit - ATTR) as u8, st.attribute[bit - ATTR]),
                STREAM..BLEND => Delta::Stream((bit - STREAM) as u8, st.stream[bit - STREAM]),
                BLEND..TEX => Delta::Blend((bit - BLEND) as u8, st.blends.raw(bit - BLEND)),
                TEX..SC => {
                    let slot = bit - TEX;
                    match self.track.tex_dep[slot] {
                        d if d != NO_DEP && d as usize >= f.dep0 => {
                            Delta::TextureDeferred(slot as u8, d - f.dep0 as u32)
                        }
                        _ => Delta::Texture(slot as u8, st.texture[slot]),
                    }
                }
                _ => {
                    groups |= 1 << (bit - SC);
                    continue;
                }
            };
            delta.push(d);
        }
        let ranges = if updates.is_empty() {
            ranges_of(&draws_v, &dispatches_v)
        } else {
            Vec::new()
        };
        let mut spans = f.spans.clone();
        spans.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(spans.len());
        for (s, e) in spans {
            match merged.last_mut() {
                Some((_, le)) if s <= *le => *le = (*le).max(e),
                _ => merged.push((s, e)),
            }
        }
        Box::new(Template {
            needs: f.needs,
            written: f.written,
            key,
            height: f.height,
            epoch: self.programs.epoch(),
            programs: f
                .programs
                .iter()
                .map(|p| (*p, self.programs.generation(*p)))
                .collect(),
            spans: merged,
            draws: draws_v,
            dispatches: dispatches_v,
            clears: self.out.clears[m.clears..].to_vec(),
            copies: self.out.copies[m.copies..].to_vec(),
            reports: self.out.reports[m.reports..].to_vec(),
            updates,
            deferred: self.track.dep_list[f.dep0..].to_vec(),
            patches,
            inherited,
            delta,
            scalars: (groups != 0).then(|| Box::new(Scalars::of(st, groups))),
            ranges,
        })
    }
}

/// The distinct uniform buffer ranges of the draws and dispatches, each with its snapshot.
pub(super) fn ranges_of(
    draws: &[Arc<DrawDetail>],
    dispatches: &[DispatchDetail],
) -> RangeSnapshots {
    let mut seen: FastMap<(u64, u32), usize> = FastMap::default();
    let mut out = RangeSnapshots::new();
    let ubos = draws
        .iter()
        .flat_map(|d| d.ubos.iter())
        .chain(dispatches.iter().flat_map(|d| d.ubos.iter()));
    for u in ubos {
        if seen.insert((u.address, u.size), out.len()).is_none() {
            out.push(((u.address, u.size), u.snapshot.clone()));
        }
    }
    out
}
