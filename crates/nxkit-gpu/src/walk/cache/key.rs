//! The key of a template: the values of the fields that its recording reads before it sets them.

use super::*;
use crate::walk::{ATTRIBUTES, BANKS};

/// What a run of fields in a [`Key`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Program,
    VertexBuffer,
    Ubo(u8),
    Sbo(u8),
    Attribute,
    Stream,
    Blend,
    Texture,
}

/// A run of `n` neighbouring fields of one kind, from the index `from`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Seg {
    kind: Kind,
    from: u16,
    n: u16,
}

/// The values that a template needs of its entry state. The fields come as runs of neighbouring slots, so that a lookup
/// compares them in tight loops. The scalar groups are compared as they are.
#[derive(Clone, Debug)]
pub(in crate::walk) struct Key {
    pub(super) segs: Vec<Seg>,
    /// The values of the runs, in order. A pair field (buffer address and size) takes two values.
    vals: Vec<u64>,
    scalars: Scalars,
    /// [`digest`] of `vals`. A state whose digest differs does not match, without a look at the fields.
    pub(super) digest: u64,
}

/// Adds `v` to the digest `h` of the values of a key.
pub(super) fn mix(h: u64, v: u64) -> u64 {
    (h.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95)
}

pub(super) fn texture_value(handle: u64, prov: u128, slot: usize) -> u64 {
    handle | u64::from(prov >> slot & 1 != 0) << 62
}

pub(super) fn attribute_value(a: Option<(u32, u32, u32)>) -> u64 {
    a.map_or(0, |(f, o, s)| {
        1 << 63 | u64::from(f) | u64::from(o) << 8 | u64::from(s) << 40
    })
}

impl Key {
    /// The key for the fields of `needs`, with the values that `st` has. `prov` tells which texture slots came from
    /// a deferred source.
    pub(super) fn of(needs: &Bits, st: &State, prov: u128) -> Self {
        let mut segs = Vec::new();
        let mut run = |kind: Kind, start: usize, len: usize| {
            let mut at = 0;
            while at < len {
                if needs.has(start + at) {
                    let from = at;
                    while at < len && needs.has(start + at) {
                        at += 1;
                    }
                    segs.push(Seg {
                        kind,
                        from: from as u16,
                        n: (at - from) as u16,
                    });
                } else {
                    at += 1;
                }
            }
        };
        run(Kind::Program, P, 6);
        run(Kind::VertexBuffer, VB, STREAMS);
        for bank in 0..BANKS as u8 {
            run(Kind::Ubo(bank), UBO + 16 * usize::from(bank), 16);
        }
        for bank in 0..BANKS as u8 {
            run(Kind::Sbo(bank), SBO + 16 * usize::from(bank), 16);
        }
        run(Kind::Attribute, ATTR, ATTRIBUTES);
        run(Kind::Stream, STREAM, STREAMS);
        run(Kind::Blend, BLEND, 8);
        run(Kind::Texture, TEX, TEXTURE_SLOTS);
        let mut vals = Vec::new();
        for s in &segs {
            let (from, n) = (usize::from(s.from), usize::from(s.n));
            match s.kind {
                Kind::Program => vals.extend_from_slice(&st.program[from..from + n]),
                Kind::VertexBuffer => st.vertex_buffer[from..from + n]
                    .iter()
                    .for_each(|(a, b)| vals.extend([*a, *b])),
                Kind::Ubo(b) => st.ubo[usize::from(b)][from..from + n]
                    .iter()
                    .for_each(|(a, s)| vals.extend([*a, u64::from(*s)])),
                Kind::Sbo(b) => st.sbuf[usize::from(b)][from..from + n]
                    .iter()
                    .for_each(|(a, s)| vals.extend([*a, u64::from(*s)])),
                Kind::Attribute => vals.extend(
                    st.attribute[from..from + n]
                        .iter()
                        .map(|a| attribute_value(*a)),
                ),
                Kind::Stream => vals.extend(
                    st.stream[from..from + n]
                        .iter()
                        .map(|(a, b)| u64::from(*a) | u64::from(*b) << 32),
                ),
                Kind::Blend => vals.extend((from..from + n).map(|i| st.blends.raw(i))),
                Kind::Texture => {
                    vals.extend((from..from + n).map(|i| texture_value(st.texture[i], prov, i)))
                }
            }
        }
        let groups = (0..32)
            .filter(|g| needs.has(SC + g))
            .fold(0u32, |m, g| m | 1 << g);
        let digest = vals.iter().fold(0, |h, v| mix(h, *v));
        Self {
            segs,
            vals,
            scalars: Scalars::of(st, groups),
            digest,
        }
    }

    /// The digest that `st` has for the fields of the key. It equals `self.digest` when `st` matches.
    pub(super) fn digest_of(&self, st: &State, prov: u128) -> u64 {
        let mut h = 0;
        for s in &self.segs {
            let (from, n) = (usize::from(s.from), usize::from(s.n));
            for f in from..from + n {
                h = match s.kind {
                    Kind::Program => mix(h, st.program[f]),
                    Kind::VertexBuffer => mix(mix(h, st.vertex_buffer[f].0), st.vertex_buffer[f].1),
                    Kind::Ubo(b) => {
                        let (a, z) = st.ubo[usize::from(b)][f];
                        mix(mix(h, a), u64::from(z))
                    }
                    Kind::Sbo(b) => {
                        let (a, z) = st.sbuf[usize::from(b)][f];
                        mix(mix(h, a), u64::from(z))
                    }
                    Kind::Attribute => mix(h, attribute_value(st.attribute[f])),
                    Kind::Stream => mix(
                        h,
                        u64::from(st.stream[f].0) | u64::from(st.stream[f].1) << 32,
                    ),
                    Kind::Blend => mix(h, st.blends.raw(f)),
                    Kind::Texture => mix(h, texture_value(st.texture[f], prov, f)),
                };
            }
        }
        h
    }

    /// The first field of the key that `st` does not have, for the `walkmiss` diagnostic.
    pub(super) fn why_not(&self, st: &State, prov: u128) -> String {
        let mut k = 0;
        for s in &self.segs {
            let (from, n) = (usize::from(s.from), usize::from(s.n));
            for i in 0..n {
                let f = from + i;
                let (ok, width) = match s.kind {
                    Kind::Program => (self.vals[k + i] == st.program[f], 1),
                    Kind::VertexBuffer => {
                        let (a, b) = st.vertex_buffer[f];
                        (
                            self.vals[k + 2 * i] == a && self.vals[k + 2 * i + 1] == b,
                            2,
                        )
                    }
                    Kind::Ubo(b) => {
                        let (a, z) = st.ubo[usize::from(b)][f];
                        (
                            self.vals[k + 2 * i] == a && self.vals[k + 2 * i + 1] == u64::from(z),
                            2,
                        )
                    }
                    Kind::Sbo(b) => {
                        let (a, z) = st.sbuf[usize::from(b)][f];
                        (
                            self.vals[k + 2 * i] == a && self.vals[k + 2 * i + 1] == u64::from(z),
                            2,
                        )
                    }
                    Kind::Attribute => (attribute_value(st.attribute[f]) == self.vals[k + i], 1),
                    Kind::Stream => {
                        let (a, b) = st.stream[f];
                        (u64::from(a) | u64::from(b) << 32 == self.vals[k + i], 1)
                    }
                    Kind::Blend => (st.blends.raw(f) == self.vals[k + i], 1),
                    Kind::Texture => (texture_value(st.texture[f], prov, f) == self.vals[k + i], 1),
                };
                let _ = width;
                if !ok {
                    return format!("{:?}[{f}]", s.kind);
                }
            }
            k += n * match s.kind {
                Kind::VertexBuffer | Kind::Ubo(_) | Kind::Sbo(_) => 2,
                _ => 1,
            };
        }
        if self.scalars.matches(st) {
            "nothing".into()
        } else {
            format!("scalars {:#x}", self.scalars.groups)
        }
    }

    /// Whether `st` has the values of the key.
    pub(super) fn matches(&self, st: &State, prov: u128) -> bool {
        let pair = |at: usize, a: u64, b: u64| self.vals[at] == a && self.vals[at + 1] == b;
        let mut k = 0;
        for s in &self.segs {
            let (from, n) = (usize::from(s.from), usize::from(s.n));
            let (ok, width) = match s.kind {
                Kind::Program => (self.vals[k..k + n] == st.program[from..from + n], 1),
                Kind::VertexBuffer => {
                    let fields = &st.vertex_buffer[from..from + n];
                    (
                        fields
                            .iter()
                            .enumerate()
                            .all(|(i, (a, b))| pair(k + 2 * i, *a, *b)),
                        2,
                    )
                }
                Kind::Ubo(b) => {
                    let fields = &st.ubo[usize::from(b)][from..from + n];
                    (
                        fields
                            .iter()
                            .enumerate()
                            .all(|(i, (a, s))| pair(k + 2 * i, *a, u64::from(*s))),
                        2,
                    )
                }
                Kind::Sbo(b) => {
                    let fields = &st.sbuf[usize::from(b)][from..from + n];
                    (
                        fields
                            .iter()
                            .enumerate()
                            .all(|(i, (a, s))| pair(k + 2 * i, *a, u64::from(*s))),
                        2,
                    )
                }
                Kind::Attribute => {
                    let fields = &st.attribute[from..from + n];
                    (
                        fields
                            .iter()
                            .zip(&self.vals[k..k + n])
                            .all(|(a, v)| attribute_value(*a) == *v),
                        1,
                    )
                }
                Kind::Stream => {
                    let fields = &st.stream[from..from + n];
                    (
                        fields
                            .iter()
                            .zip(&self.vals[k..k + n])
                            .all(|((a, b), v)| u64::from(*a) | u64::from(*b) << 32 == *v),
                        1,
                    )
                }
                Kind::Blend => (
                    (from..from + n)
                        .zip(&self.vals[k..k + n])
                        .all(|(i, v)| st.blends.raw(i) == *v),
                    1,
                ),
                Kind::Texture => (
                    (from..from + n)
                        .zip(&self.vals[k..k + n])
                        .all(|(i, v)| texture_value(st.texture[i], prov, i) == *v),
                    1,
                ),
            };
            if !ok {
                return false;
            }
            k += n * width;
        }
        self.scalars.matches(st)
    }
}
