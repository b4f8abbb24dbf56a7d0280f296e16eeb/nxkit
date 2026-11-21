//! Readers of the bound state of one bank: uniform and storage buffers, textures, samplers and images of a stage.

use super::*;

/// The number of bound buffers in a bank of `(address, size)` slots.
pub(super) fn bound_count(bank: &[(u64, u32); UBOS]) -> usize {
    bank.iter().filter(|(a, s)| *a != 0 && *s != 0).count()
}

/// Adds the storage buffers of the bank (0 vertex, 1 fragment, 2 compute, 3 task, 4 mesh) that are bound to `out`. The
/// shader reads and writes them in place, so they carry no snapshot.
pub(super) fn sbufs_into(st: &State, bank: usize, stage: Stage, out: &mut Vec<UboDetail>) {
    out.extend(
        st.sbuf[bank]
            .iter()
            .enumerate()
            .filter(|(_, (a, s))| *a != 0 && *s != 0)
            .map(|(i, (a, s))| UboDetail {
                stage,
                slot: i as u32,
                address: *a,
                size: *s,
                snapshot: None,
            }),
    );
}

/// The storage buffers of the bank (0 vertex, 1 fragment, 2 compute, 3 task, 4 mesh) that are bound.
pub(super) fn sbufs(st: &State, bank: usize, stage: Stage) -> Vec<UboDetail> {
    let mut out = Vec::with_capacity(bound_count(&st.sbuf[bank]));
    sbufs_into(st, bank, stage, &mut out);
    out
}

/// The 32 slots of the bank (0 vertex, 1 fragment, 2 compute, 3 the other stages) in a table of the bank layout.
fn bank<T>(table: &[T; TEXTURE_SLOTS], bank: usize) -> &[T] {
    table.get(bank * 32..bank * 32 + 32).unwrap_or(&[])
}

/// The number of texture handles of the bank that are bound, separate textures included.
pub(super) fn texture_count(st: &State, bank_index: usize) -> usize {
    bank(&st.texture, bank_index)
        .iter()
        .chain(bank(&st.separate_texture, bank_index))
        .filter(|h| **h != NO_HANDLE)
        .count()
}

/// Adds the texture handles of the bank (0 vertex, 1 fragment, 2 compute, 3 the other stages) that are bound to `out`,
/// the separate textures from [`SEPARATE_TEXTURE_SLOT0`].
pub(super) fn textures_into(
    st: &State,
    bank_index: usize,
    stage: Stage,
    out: &mut Vec<TextureBind>,
) {
    let bound = |table, first: u32| {
        bank(table, bank_index)
            .iter()
            .enumerate()
            .filter(|(_, h)| **h != NO_HANDLE)
            .map(move |(i, handle)| TextureBind {
                stage,
                slot: first + i as u32,
                handle: *handle,
            })
    };
    out.extend(bound(&st.texture, 0));
    out.extend(bound(&st.separate_texture, SEPARATE_TEXTURE_SLOT0));
}

/// Adds the separate samplers of the bank that are bound to `out`.
pub(super) fn samplers_into(
    st: &State,
    bank_index: usize,
    stage: Stage,
    out: &mut Vec<SamplerBind>,
) {
    out.extend(
        bank(&st.separate_sampler, bank_index)
            .iter()
            .enumerate()
            .filter(|(_, id)| **id != 0)
            .map(|(i, id)| SamplerBind {
                stage,
                slot: i as u32,
                id: *id,
            }),
    );
}

/// The separate samplers of the bank that are bound.
pub(super) fn samplers(st: &State, bank_index: usize, stage: Stage) -> Vec<SamplerBind> {
    let mut out = Vec::new();
    samplers_into(st, bank_index, stage, &mut out);
    out
}

/// Adds the image handles of the bank (same layout as the textures) that are bound to `out`.
pub(super) fn images_into(st: &State, bank_index: usize, stage: Stage, out: &mut Vec<TextureBind>) {
    let slots = st
        .image
        .get(bank_index * 32..bank_index * 32 + 32)
        .unwrap_or(&[]);
    out.extend(
        slots
            .iter()
            .enumerate()
            .filter(|(_, h)| **h != NO_HANDLE)
            .map(|(i, handle)| TextureBind {
                stage,
                slot: i as u32,
                handle: *handle,
            }),
    );
}

/// The image handles of the bank that are bound.
pub(super) fn images(st: &State, bank_index: usize, stage: Stage) -> Vec<TextureBind> {
    let mut out = Vec::new();
    images_into(st, bank_index, stage, &mut out);
    out
}

/// The texture handles of the bank (0 vertex, 1 fragment, 2 compute, 3 the other stages) that are bound.
pub(super) fn textures(st: &State, bank_index: usize, stage: Stage) -> Vec<TextureBind> {
    let mut out = Vec::with_capacity(texture_count(st, bank_index));
    textures_into(st, bank_index, stage, &mut out);
    out
}
