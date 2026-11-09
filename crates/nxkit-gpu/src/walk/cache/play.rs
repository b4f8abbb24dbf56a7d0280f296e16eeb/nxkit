//! The play of a template on a hit.

use super::build::ranges_of;
use super::*;

impl Walker {
    /// Plays the template: the outputs, the memory writes, the state it leaves, and the logs for the recording that
    /// calls it.
    pub(super) fn play<M: GuestMem>(&mut self, mem: &mut M, st: &mut State, t: &mut Template) {
        let dep_base = self.track.dep_list.len();
        let mut moved = false;
        for d in &mut t.deferred {
            let handle = deferred_handle(&*mem, d.src, d.k, d.tex_base, d.samp_base)
                .map_or(0, |(_, _, h)| h);
            moved |= handle != d.handle;
            self.track.dep_list.push(DepSrc { handle, ..*d });
        }
        let (d0, p0) = (self.out.draws.len(), self.out.dispatches.len());
        self.out.clears.extend_from_slice(&t.clears);
        self.out.copies.extend_from_slice(&t.copies);
        self.out.reports.extend_from_slice(&t.reports);
        if t.updates.is_empty() && self.ranges_hold(mem, t) {
            self.out.draws.extend(t.draws.iter().cloned());
            self.out.dispatches.extend(t.dispatches.iter().cloned());
        } else {
            self.play_in_order(mem, t);
        }
        if moved {
            self.patch_textures(t, dep_base, d0, p0);
        }
        if self.track.tracking() {
            for u in &t.updates {
                self.track
                    .update(&u.ext, d0 + u.draws as usize, p0 + u.dispatches as usize);
            }
            for p in &t.patches {
                let base = if p.dispatch { p0 } else { d0 };
                self.track.patches.push(Patch {
                    idx: (base + p.idx as usize) as u32,
                    dep: (dep_base as u32) + p.dep,
                    ..*p
                });
            }
            for p in &t.inherited {
                let base = if p.dispatch { p0 } else { d0 };
                let dep = self
                    .track
                    .tex_dep
                    .get(usize::from(p.slot))
                    .copied()
                    .unwrap_or(NO_DEP);
                if dep != NO_DEP {
                    self.track.patches.push(Patch {
                        idx: (base + p.idx as usize) as u32,
                        dep,
                        ..*p
                    });
                }
            }
        }
        self.apply_delta(st, t, dep_base);
        if let Some(parent) = self.track.open.last_mut() {
            parent.absorb(
                &t.needs,
                &t.written,
                t.programs.iter().map(|(p, _)| *p),
                &t.spans,
                t.height,
                false,
            );
        }
    }

    /// Whether the uniform buffer ranges of a template without updates still have the snapshots that its draws hold.
    fn ranges_hold<M: GuestMem>(&self, mem: &M, t: &Template) -> bool {
        t.ranges
            .iter()
            .all(|((a, s), snap)| same(&self.snapshot(mem, *a, *s), snap))
    }

    /// Plays the draws and dispatches in the order of the walk, with the inline updates between them. A draw whose
    /// snapshots no longer match gets new ones, and the template keeps the new draw.
    fn play_in_order<M: GuestMem>(&mut self, mem: &mut M, t: &mut Template) {
        let (mut d, mut p) = (0usize, 0usize);
        let n = t.updates.len();
        for u in 0..=n {
            let (de, pe) = match t.updates.get(u) {
                Some(up) => (up.draws as usize, up.dispatches as usize),
                None => (t.draws.len(), t.dispatches.len()),
            };
            for k in d..de {
                let a = self.fresh_draw(mem, &t.draws[k]);
                t.draws[k] = a.clone();
                self.out.draws.push(a);
            }
            for k in p..pe {
                let x = self.fresh_dispatch(mem, &t.dispatches[k]);
                t.dispatches[k] = x.clone();
                self.out.dispatches.push(x);
            }
            (d, p) = (de, pe);
            if let Some(up) = t.updates.get(u) {
                let mut bytes = std::mem::take(&mut self.update_bytes);
                self.write_update(mem, &up.ext, &mut bytes);
                self.update_bytes = bytes;
            }
        }
        if t.updates.is_empty() {
            t.ranges = ranges_of(&t.draws, &t.dispatches);
        }
    }

    /// The draw with the snapshots that its uniform buffers have now.
    fn fresh_draw<M: GuestMem>(&self, mem: &M, a: &Arc<DrawDetail>) -> Arc<DrawDetail> {
        if a.ubos
            .iter()
            .all(|u| same(&self.snapshot(mem, u.address, u.size), &u.snapshot))
        {
            return a.clone();
        }
        let mut d = (**a).clone();
        for u in &mut d.ubos {
            u.snapshot = self.snapshot(mem, u.address, u.size);
        }
        Arc::new(d)
    }

    /// The dispatch with the snapshots that its uniform buffers have now.
    fn fresh_dispatch<M: GuestMem>(&self, mem: &M, x: &DispatchDetail) -> DispatchDetail {
        let mut x = x.clone();
        for u in &mut x.ubos {
            u.snapshot = self.snapshot(mem, u.address, u.size);
        }
        x
    }

    /// Gives the draws and dispatches that took a handle from a deferred source that changed the new handle.
    fn patch_textures(&mut self, t: &mut Template, dep_base: usize, d0: usize, p0: usize) {
        for p in &t.patches {
            let handle = self.track.dep_list[dep_base + p.dep as usize].handle;
            let pos = usize::from(p.pos);
            if p.dispatch {
                let at = p0 + p.idx as usize;
                if let Some(x) = self
                    .out
                    .dispatches
                    .get_mut(at)
                    .filter(|x| x.textures[pos].handle != handle)
                {
                    x.textures[pos].handle = handle;
                    t.dispatches[p.idx as usize].textures[pos].handle = handle;
                }
            } else {
                let at = d0 + p.idx as usize;
                if let Some(a) = self
                    .out
                    .draws
                    .get_mut(at)
                    .filter(|a| a.textures[pos].handle != handle)
                {
                    let mut d = (**a).clone();
                    d.textures[pos].handle = handle;
                    *a = Arc::new(d);
                    t.draws[p.idx as usize] = a.clone();
                }
            }
        }
        for (i, d) in t.deferred.iter_mut().enumerate() {
            d.handle = self.track.dep_list[dep_base + i].handle;
        }
    }

    /// Sets the fields that the recording set to the values it left.
    fn apply_delta(&mut self, st: &mut State, t: &Template, dep_base: usize) {
        for d in &t.delta {
            match *d {
                Delta::Program(i, v) => st.program[usize::from(i)] = v,
                Delta::VertexBuffer(i, v) => st.vertex_buffer[usize::from(i)] = v,
                Delta::Ubo(i, v) => st.ubo[usize::from(i) / 16][usize::from(i) % 16] = v,
                Delta::Sbo(i, v) => st.sbuf[usize::from(i) / 16][usize::from(i) % 16] = v,
                Delta::Attribute(i, v) => st.attribute[usize::from(i)] = v,
                Delta::Stream(i, v) => st.stream[usize::from(i)] = v,
                Delta::Blend(i, v) => st.blends.set_raw(usize::from(i), v),
                Delta::Texture(i, v) => {
                    st.texture[usize::from(i)] = v;
                    self.track.tex_dep[usize::from(i)] = NO_DEP;
                    self.track.prov &= !(1u128 << i);
                }
                Delta::TextureDeferred(i, rel) => {
                    let at = dep_base + rel as usize;
                    st.texture[usize::from(i)] = self.track.dep_list[at].handle;
                    self.track.tex_dep[usize::from(i)] = at as u32;
                    self.track.prov |= 1u128 << i;
                }
            }
        }
        if let Some(s) = &t.scalars {
            s.apply(st);
        }
    }
}
