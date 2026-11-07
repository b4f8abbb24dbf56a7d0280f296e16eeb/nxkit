//! The program group: `nvnProgram*` and the program builder.
//!
//! A game gives a program its shaders with `nvnProgramSetShaders`. Each stage is a code address and a control section.
//! The host does not run the code. The shader step already translated every shader to a Metal function, and the
//! `_b.json` of the step names it by a key. This group computes the key of each stage, as `nxnative` does:
//! the first 8 bytes of `sha256(key bytes of the control section || code)`. `nxb-split` computes the same key
//! (`shaders::skey` and `shaders::key_ctl`).
//!
//! The key bytes of the control section:
//! - NVN1: the first `own` bytes, where `own` is the u32 at +0x20 when it lies in `0x760..0x880` and 0x880 otherwise.
//!   The code length is the u32 at +0x704. `Settings::shader_key_skip` sets a byte range to zero.
//! - NVN2 (magic 0x98761234 at +0, version 0xc9 at +4): the first 0x48 bytes with the u32 at +0x30 set to zero.
//!   The code length is the u32 at +0x44.
//!
//! The code is part of the hash when its address is not null and its length is in `1..2^24`.
//! A key exists after `SetShaders` when the code lies in guest memory. Mario sets some programs with an address that
//! holds no code yet and fills it before the first draw. For such a stage the key bytes of the control section are
//! copied at the call, because the control section is valid only during the call, and the key is computed at the first
//! use ([`stages_of`]).
//!
//! The keys live in `Gpu::programs`, keyed by the guest address of the `NVNprogram`, because a game may allocate a
//! program object of fewer than 0xC0 bytes.

use nxkit_abi::guest;
use nxkit_nvn::backend::{ShaderKey, StageKey};

use super::abi::{AbiResult, Obj, fatal, guard, log_once, magic, ret_bool, size, table};
use super::ctx::{Settings, gpu};

/// The magic of an initialised program.
const PROG: u32 = magic(b'P', b'R', b'O', b'G');
/// The most stages that a program has.
const MAX_STAGES: i32 = 6;
/// The magic of an NVN2 control section, as bytes.
const NVN2_MAGIC: [u8; 4] = [0x34, 0x12, 0x76, 0x98];
/// The bytes of an NVN2 control section that the key covers.
const NVN2_KEY_LEN: usize = 0x48;
/// The bytes of an NVN1 control section that the key covers at most.
const NVN1_KEY_MAX: usize = 0x880;
/// The code of a stage is at most this long.
const CODE_LIMIT: u32 = 1 << 24;

/// One stage of a program.
#[derive(Clone, Debug)]
pub(crate) struct Stage {
    /// The position in the array of `SetShaders`.
    pub(crate) index: u8,
    /// The host address of the code area.
    pub(crate) code: u64,
    /// The length of the code area. Zero when the control section says there is none.
    pub(crate) code_len: u32,
    /// The key bytes of the control section. They are kept while `key` is None.
    key_bytes: Vec<u8>,
    /// The key. None until the code lies in guest memory.
    pub(crate) key: Option<ShaderKey>,
}

/// What the host knows of a program.
#[derive(Clone, Debug, Default)]
pub(crate) struct ProgramRecord {
    /// The stages in the order of the game.
    pub(crate) stages: Vec<Stage>,
}

/// True when `head` is the start of an NVN2 control section.
fn is_nvn2(head: &[u8]) -> bool {
    head.len() >= 8 && head[..4] == NVN2_MAGIC && head[4..8] == [0xc9, 0, 0, 0]
}

fn le32(b: &[u8], off: usize) -> u32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[off..off + 4]);
    u32::from_le_bytes(a)
}

/// How many bytes of the control section the key covers, from its first 0x48 bytes.
pub(crate) fn section_len(head: &[u8; NVN2_KEY_LEN]) -> usize {
    if is_nvn2(head) {
        return NVN2_KEY_LEN;
    }
    let own = le32(head, 0x20) as usize;
    if (0x760..NVN1_KEY_MAX).contains(&own) {
        own
    } else {
        NVN1_KEY_MAX
    }
}

/// The key bytes and the code length of a control section. `section` holds [`section_len`] bytes.
pub(crate) fn key_parts(section: &[u8], skip: Option<(u32, u32)>) -> (Vec<u8>, u32) {
    if is_nvn2(section) {
        let mut key = section[..NVN2_KEY_LEN].to_vec();
        key[0x30..0x34].fill(0);
        return (key, le32(section, 0x44));
    }
    let mut key = section.to_vec();
    let code_len = if key.len() >= 0x708 {
        le32(&key, 0x704)
    } else {
        0
    };
    if let Some((off, len)) = skip {
        let (off, len) = (off as usize, len as usize);
        if len > 0 && off + len <= key.len() {
            key[off..off + len].fill(0);
        }
    }
    (key, code_len)
}

/// The key of a stage.
pub(crate) fn hash_key(key_bytes: &[u8], code: Option<&[u8]>) -> ShaderKey {
    let mut h = nxb_model::Sha256::new();
    h.update(key_bytes);
    if let Some(c) = code {
        h.update(c);
    }
    let d = h.finalize();
    let mut k = [0u8; 8];
    k.copy_from_slice(&d.0[..8]);
    ShaderKey(k)
}

/// True when the stage has code that the key covers.
fn has_code(code: u64, len: u32) -> bool {
    code != 0 && len != 0 && len < CODE_LIMIT
}

/// Reads the control section at `control` and builds the stage `index`.
fn read_stage(index: usize, data: u64, control: u64, settings: &Settings) -> AbiResult<Stage> {
    let mut head = [0u8; NVN2_KEY_LEN];
    guest::read(control, &mut head)?;
    let mut section = vec![0u8; section_len(&head)];
    guest::read(control, &mut section)?;
    let (key_bytes, code_len) = key_parts(&section, settings.shader_key_skip);
    let covered = has_code(data, code_len);
    let key = if covered && guest::check(data, u64::from(code_len)).is_ok() {
        Some(code_key(&key_bytes, data, code_len)?)
    } else {
        None
    };
    Ok(Stage {
        index: index as u8,
        code: data,
        code_len,
        key_bytes: if key.is_some() { Vec::new() } else { key_bytes },
        key,
    })
}

fn code_key(key_bytes: &[u8], data: u64, len: u32) -> AbiResult<ShaderKey> {
    Ok(guest::with_slice(data, u64::from(len), |c| {
        hash_key(key_bytes, Some(c))
    })?)
}

fn stage_key(s: &Stage) -> StageKey {
    StageKey {
        index: s.index,
        key: s.key,
        code: s.code,
        code_len: s.code_len,
    }
}

/// The stages of the program at the guest address `program`, with the key of each. A key that was not computable at
/// `SetShaders` is computed now and kept. None when the game set no shaders on that program.
///
/// The command buffer group calls it when a program is bound. A stage whose code is still not in guest memory ends the
/// game with the name `nvnCommandBufferBindProgram`'s caller gives in `name`.
pub(crate) fn stages_of(name: &str, program: u64) -> Option<Vec<StageKey>> {
    let g = gpu(name);
    let mut rec = g.programs.get(program)?;
    let mut changed = false;
    for s in &mut rec.stages {
        if s.key.is_some() {
            continue;
        }
        let key = if has_code(s.code, s.code_len) {
            match code_key(&s.key_bytes, s.code, s.code_len) {
                Ok(k) => k,
                Err(e) => fatal(
                    name,
                    format_args!("the code of a shader stage is not readable: {e}"),
                ),
            }
        } else {
            hash_key(&s.key_bytes, None)
        };
        s.key = Some(key);
        s.key_bytes = Vec::new();
        changed = true;
    }
    let keys = rec.stages.iter().map(stage_key).collect();
    if changed {
        g.programs.with(program, |r| *r = rec);
    }
    Some(keys)
}

/// Reads the stage array and records the stages. `stride` is 16 (`NVNshaderData`) or 24 (the NVN 55 extension).
fn set_shaders(name: &'static str, p: u64, n: i32, sd: u64, stride: u64) -> AbiResult<u32> {
    if !(0..=MAX_STAGES).contains(&n) {
        eprintln!("[RED] NVN {name}: stage count {n}");
        return Ok(0);
    }
    let g = gpu(name);
    let mut stages = Vec::with_capacity(n as usize);
    for i in 0..n as u64 {
        let at = Obj(sd).at(i * stride);
        let (data, control) = (at.u64(0)?, at.u64(8)?);
        if stride > 16 && at.u64(16)? != 0 {
            log_once(
                name,
                format_args!("{name}: the extension field is not zero, the host does not use it"),
            );
        }
        stages.push(read_stage(i as usize, data, control, g.settings())?);
    }
    let keys: Vec<StageKey> = stages.iter().map(stage_key).collect();
    Obj(p).set_u32(4, n as u32)?;
    g.programs.insert(p, ProgramRecord { stages });
    g.backend().program_set(p, &keys);
    Ok(1)
}

extern "C" fn program_builder_set_defaults(b: u64) {
    guard("nvnProgramBuilderSetDefaults", || Obj(b).zero(24));
}

extern "C" fn program_builder_set_device(b: u64, dev: u64) {
    guard("nvnProgramBuilderSetDevice", || Obj(b).set_u64(0, dev));
}

extern "C" fn program_builder_set_shaders(b: u64, n: i32, sd: u64) {
    guard("nvnProgramBuilderSetShaders", || {
        Obj(b).set_u64(8, sd)?;
        Obj(b).set_i32(16, n)
    });
}

extern "C" fn program_initialize(p: u64, _dev: u64) -> u32 {
    guard("nvnProgramInitialize", || {
        init(p)?;
        Ok(1)
    })
}

fn init(p: u64) -> AbiResult<()> {
    let o = Obj(p);
    o.zero(size::PROGRAM)?;
    o.set_u32(0, PROG)?;
    gpu("nvnProgramInitialize").programs.remove(p);
    Ok(())
}

extern "C" fn program_initialize_from_builder(p: u64, b: u64) -> u32 {
    guard("nvnProgramInitializeFromBuilder", || {
        init(p)?;
        let bo = Obj(b);
        let (sd, n) = (bo.u64(8)?, bo.i32(16)?);
        if n == 0 {
            return Ok(1);
        }
        set_shaders("nvnProgramInitializeFromBuilder", p, n, sd, 16)
    })
}

extern "C" fn program_finalize(p: u64) {
    guard("nvnProgramFinalize", || {
        let g = gpu("nvnProgramFinalize");
        if g.programs.remove(p).is_some() {
            g.backend().program_released(p);
        }
        Obj(p).set_u32(0, 0)
    });
}

extern "C" fn program_set_debug_label(_p: u64, _label: u64) {}

extern "C" fn program_set_shaders(p: u64, n: i32, sd: u64) -> u32 {
    guard("nvnProgramSetShaders", || {
        set_shaders("nvnProgramSetShaders", p, n, sd, 16)
    })
}

extern "C" fn program_set_shaders_ext(p: u64, n: i32, sd: u64) -> u32 {
    guard("nvnProgramSetShadersExt", || {
        set_shaders("nvnProgramSetShadersExt", p, n, sd, 24)
    })
}

extern "C" fn program_set_sample_shading(_p: u64, mode: i32) -> u32 {
    log_once(
        "nvnProgramSetSampleShading",
        format_args!(
            "nvnProgramSetSampleShading mode {mode}: Metal shades per sample only where the shader reads the sample index"
        ),
    );
    ret_bool(true)
}

/// The functions of the group.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    table![
        "nvnProgramBuilderSetDevice" => program_builder_set_device as extern "C" fn(u64, u64),
        "nvnProgramBuilderSetDefaults" => program_builder_set_defaults as extern "C" fn(u64),
        "nvnProgramBuilderSetShaders" => program_builder_set_shaders as extern "C" fn(u64, i32, u64),
        "nvnProgramInitialize" => program_initialize as extern "C" fn(u64, u64) -> u32,
        "nvnProgramInitializeFromBuilder" => program_initialize_from_builder as extern "C" fn(u64, u64) -> u32,
        "nvnProgramFinalize" => program_finalize as extern "C" fn(u64),
        "nvnProgramSetDebugLabel" => program_set_debug_label as extern "C" fn(u64, u64),
        "nvnProgramSetShaders" => program_set_shaders as extern "C" fn(u64, i32, u64) -> u32,
        "nvnProgramSetShadersExt" => program_set_shaders_ext as extern "C" fn(u64, i32, u64) -> u32,
        "nvnProgramSetSampleShading" => program_set_sample_shading as extern "C" fn(u64, i32) -> u32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nvn1_section(own: u32, code_len: u32) -> Vec<u8> {
        let mut s = vec![0u8; NVN1_KEY_MAX];
        s[0x20..0x24].copy_from_slice(&own.to_le_bytes());
        s[0x704..0x708].copy_from_slice(&code_len.to_le_bytes());
        s[0x10] = 0xAB;
        s
    }

    fn nvn2_section(code_len: u32) -> Vec<u8> {
        let mut s = vec![0u8; 0xe0];
        s[..4].copy_from_slice(&NVN2_MAGIC);
        s[4] = 0xc9;
        s[0x30..0x34].copy_from_slice(&0xd8u32.to_le_bytes());
        s[0x44..0x48].copy_from_slice(&code_len.to_le_bytes());
        s
    }

    fn head(s: &[u8]) -> [u8; NVN2_KEY_LEN] {
        let mut h = [0u8; NVN2_KEY_LEN];
        h.copy_from_slice(&s[..NVN2_KEY_LEN]);
        h
    }

    #[test]
    fn an_nvn1_section_uses_its_own_length_when_it_is_plausible() {
        assert_eq!(section_len(&head(&nvn1_section(0x850, 0))), 0x850);
        assert_eq!(section_len(&head(&nvn1_section(0x880, 0))), 0x880);
        assert_eq!(section_len(&head(&nvn1_section(0x75f, 0))), 0x880);
        assert_eq!(section_len(&head(&nvn1_section(0x2000, 0))), 0x880);
    }

    #[test]
    fn an_nvn2_section_keys_on_its_first_0x48_bytes_with_the_u32_at_0x30_zeroed() {
        let s = nvn2_section(0x300);
        assert_eq!(section_len(&head(&s)), 0x48);
        let (key, code_len) = key_parts(&s[..0x48], None);
        assert_eq!(code_len, 0x300);
        assert_eq!(key.len(), 0x48);
        assert_eq!(&key[0x30..0x34], &[0, 0, 0, 0]);
        assert_eq!(&key[..4], &NVN2_MAGIC);
    }

    #[test]
    fn the_code_length_of_an_nvn1_section_is_at_0x704() {
        let s = nvn1_section(0x880, 0x1230);
        let (key, code_len) = key_parts(&s, None);
        assert_eq!(code_len, 0x1230);
        assert_eq!(key, s);
    }

    #[test]
    fn the_skip_range_is_hashed_as_zero_and_a_range_past_the_end_is_ignored() {
        let s = nvn1_section(0x880, 0);
        let (key, _) = key_parts(&s, Some((0x10, 4)));
        assert_eq!(key[0x10], 0);
        assert_eq!(key.len(), s.len());
        let (same, _) = key_parts(&s, Some((0x87e, 8)));
        assert_eq!(same, s);
    }

    #[test]
    fn the_key_is_the_first_8_bytes_of_the_sha256_of_control_then_code() {
        let ctl = [1u8, 2, 3];
        let code = [9u8, 8, 7, 6];
        let joined = [1u8, 2, 3, 9, 8, 7, 6];
        let want = nxb_model::sha256(&joined).0;
        let k = hash_key(&ctl, Some(&code));
        assert_eq!(&k.0, &want[..8]);
        assert_eq!(k.to_hex().len(), 16);
        let none = nxb_model::sha256(&ctl).0;
        assert_eq!(&hash_key(&ctl, None).0, &none[..8]);
    }

    #[test]
    fn code_covers_the_key_only_inside_the_limit() {
        assert!(!has_code(0, 10));
        assert!(!has_code(0x1000, 0));
        assert!(!has_code(0x1000, CODE_LIMIT));
        assert!(has_code(0x1000, CODE_LIMIT - 1));
    }

    #[test]
    fn the_group_exports_every_name_of_its_row() {
        let mut got: Vec<&str> = procs().iter().map(|(n, _)| *n).collect();
        let mut want: Vec<&str> = nxkit_nvn::entries::FFI_GROUPS
            .iter()
            .filter(|(g, _)| *g == "program")
            .flat_map(|(_, names)| names.iter().copied())
            .collect();
        got.sort_unstable();
        want.sort_unstable();
        assert_eq!(got, want);
    }
}
