//! Synthetic emit tests of single instructions (no corpus needed).
#![cfg(test)]

use super::emit_test_util::*;

/// `FSET.GT R1, R2, R3` with PT as the predicate input, optionally `.FTZ`.
fn fset_r(ftz: bool) -> u64 {
    let w = 0x5800u64 << 48 | 1 | 2 << 8 | 3 << 20 | PRED7 | 7 << 39 | 4 << 48;
    if ftz { w | 1 << 55 } else { w }
}

/// Number of lines of the dump that carry the operation `op` (the hex code of the dump).
fn count(text: &str, op: &str) -> usize {
    let pat = format!("  op {op} ");
    text.lines().filter(|l| l.starts_with(&pat)).count()
}

#[test]
fn fset_ftz_flushes_both_operands() {
    let plain = maxwell_cs_text(&[fset_r(false)]).expect("emit");
    let ftz = maxwell_cs_text(&[fset_r(true)]).expect("emit");
    // FPCompareLess FP32 (0x10025) against the smallest normal, once per operand
    assert_eq!(count(&plain, "10025"), 0);
    assert_eq!(count(&ftz, "10025"), 2);
    assert!(ftz.contains("#800000"));
}

/// `FSET.GT.FTZ R1, R2, imm` with PT as the predicate input. `imm` is the 19 top bits of the float.
fn fset_imm(imm: u64) -> u64 {
    0x3000u64 << 48 | 1 | 2 << 8 | imm << 20 | PRED7 | 7 << 39 | 4 << 48 | 1 << 55
}

#[test]
fn fset_ftz_against_a_normal_constant_needs_no_flush() {
    // 0.5: a denormal is on the same side of it as zero, so the register needs no flush
    let normal = maxwell_cs_text(&[fset_imm(0x3f000)]).expect("emit");
    assert_eq!(count(&normal, "10025"), 0, "{normal}");
    // 0.0: a denormal is not on the same side as zero, so the register is flushed
    let zero = maxwell_cs_text(&[fset_imm(0)]).expect("emit");
    assert_eq!(count(&zero, "10025"), 1, "{zero}");
}

#[test]
fn ldc_is_selects_among_the_runtime_banks() {
    // LDC R1, c[3][R2 + 0x10] with .IS: the bank is 3 + (R2 >> 16), the offset R2 & 0xffff. The banks are c1 and c3.
    let t = ampere_cs_text(&[ampere_ldc(1, 2, 3, 0x10, 2)]).expect("emit");
    assert_eq!(count(&t, "4a"), 2);
    assert_eq!(count(&t, "20"), 2);
    assert!(t.contains("op 14 0 0 0 d=l2, s=R1:2,#ffff"));
}

#[test]
fn ldc_is_reaches_the_banks_other_reads_name() {
    // another LDC reads c5 (and c0, which is left out): the run-time bank can be c1, c3 or c5
    let t = ampere_cs_text(&[
        ampere_ldc(1, 2, 3, 0x10, 2),
        ampere_ldc(4, 255, 5, 0x20, 0),
        ampere_ldc(6, 255, 0, 0x30, 0),
    ])
    .expect("emit");
    // three selects for the .IS read, one plain load for each of the others
    assert_eq!(count(&t, "20"), 3);
    assert_eq!(count(&t, "4a"), 5);
}

#[test]
fn ldc_il_is_refused() {
    let r = ampere_cs_text(&[ampere_ldc(1, 2, 3, 0x10, 1)]);
    assert!(r.is_err_and(|e| e.contains(".IL address mode")));
}

/// `VOTEU` (`uniform`) or `VOTE` with the vote mode `mode` of the lane predicate P0, the result in predicate 2.
fn vote(uniform: bool, mode: u64) -> (u64, u64) {
    let op = if uniform { 0x886 } else { 0x806 };
    let lo = op | APT | 0xff << 16;
    let hi = (mode << (72 - 64)) | (2 << (81 - 64));
    (lo, hi)
}

#[test]
fn voteu_writes_the_uniform_predicate() {
    let t = ampere_cs_text(&[vote(true, 1)]).expect("emit");
    assert!(t.contains("d=R4:2,"));
    assert!(!t.contains("d=R2:2,"));
    let t = ampere_cs_text(&[vote(false, 1)]).expect("emit");
    assert!(t.contains("d=R2:2,"));
}

/// `BMSK R1, R2, R3`, with `.W` when `w`.
fn bmsk(w: bool) -> (u64, u64) {
    let lo = 0x01b | 1 << 9 | APT | 1 << 16 | 2 << 24 | 3 << 32;
    (lo, if w { 1 << (75 - 64) } else { 0 })
}

#[test]
fn bmsk_clamps_unless_w() {
    let count_geu = |t: &str| count(t, "23");
    let plain = ampere_cs_text(&[bmsk(false)]).expect("emit");
    let wrap = ampere_cs_text(&[bmsk(true)]).expect("emit");
    // the field reaching bit 32, and with the clamping form a and b at least 32 as well
    assert_eq!(count_geu(&plain), 3);
    assert_eq!(count_geu(&wrap), 1);
}

/// `PIXLD.COVERED R1` (0x925 with 78..81 = 1 and no predicate result).
fn pixld() -> (u64, u64) {
    (0x925 | APT | 1 << 16, 1 << (78 - 64) | 7 << (81 - 64))
}

#[test]
fn pixld_reads_the_helper_invocation() {
    let t = ampere_ps_text(&[pixld()]).expect("emit");
    // Load Input, IoVariable 43, and a select of 0 and 1
    assert!(t.contains("op 4a 1 0 0 d=l0, s=#2b,"), "{t}");
}

#[test]
fn pixld_is_refused_outside_a_pixel_shader() {
    let r = ampere_cs_text(&[pixld()]);
    assert!(r.is_err_and(|e| e.contains("0x925 form 1")));
}

#[test]
fn a_pixel_shader_reads_its_depth_after_four_registers_per_target() {
    let t = ampere_ps_depth_text(0x10f, &[]).expect("emit");
    let depth = t
        .lines()
        .find(|l| l.contains("s=#b,"))
        .unwrap_or_else(|| panic!("no depth store\n{t}"));
    assert!(depth.contains("R1:9"), "{depth}");
    let t = ampere_ps_depth_text(0xf, &[]).expect("emit");
    assert!(
        t.lines().any(|l| l.contains("s=#b,") && l.contains("R1:5")),
        "{t}"
    );
}

/// `UP2UR UR4, URZ, mask`.
fn up2ur(mask: u64) -> (u64, u64) {
    (0x883 | APT | 4 << 16 | 63 << 24 | mask << 32, 0)
}

#[test]
fn up2ur_packs_the_masked_uniform_predicates() {
    let t = ampere_cs_text(&[up2ur(0x41)]).expect("emit");
    assert_eq!(count(&t, "2a"), 2, "{t}");
    assert!(t.contains("R4:0") && t.contains("#40"), "{t}");
}

#[test]
fn up2ur_refuses_a_mask_past_the_predicates() {
    let r = ampere_cs_text(&[up2ur(0x100)]);
    assert!(r.is_err_and(|e| e.contains("mask past the predicates")));
}

/// A 2D texture instruction of opcode `op` (Rd R4, Ra R8, Rb R12, a uniform handle in UR2 for the 0xf.. forms), all four components.
fn tex2d(op: u64) -> (u64, u64) {
    let lo = op | APT | 4 << 16 | 8 << 24 | 12 << 32 | 2 << 40 | 1 << 61;
    (lo, 0xf << 8 | 7 << 17)
}

#[test]
fn tld4_takes_a_register_handle_from_the_second_block() {
    let plain = ampere_cs_text(&[tex2d(0xb63)]).expect("emit");
    let t = ampere_cs_text(&[tex2d(0x364)]).expect("emit");
    assert!(plain.contains("s=l0,l1,#0,"), "{plain}");
    // the handle R12 comes first
    assert!(t.contains("d=l2, s=R1:12,"), "{t}");
    assert!(t.contains("s=l2,l0,l1,#0,"), "{t}");
}

#[test]
fn tld4_and_txd_take_a_uniform_register_handle() {
    let tld4 = ampere_cs_text(&[tex2d(0xf63)]).expect("emit");
    assert!(tld4.contains("s=l0,l1,l2,#0,"), "{tld4}");
    assert!(tld4.contains("d=l0, s=R3:2,"), "{tld4}");
    let txd = ampere_cs_text(&[tex2d(0xf6c)]).expect("emit");
    // the handle, then the two coordinates, then the derivatives
    assert!(txd.contains("s=l0,l1,l2,l3,l4,l5,l6,"), "{txd}");
    assert!(txd.contains("d=l0, s=R3:2,"), "{txd}");
}

#[test]
fn tld_takes_a_register_handle() {
    let (lo, hi) = tex2d(0x367);
    let t = ampere_cs_text(&[(lo, hi | 1 << (87 - 64))]).expect("emit");
    assert!(t.contains("d=l0, s=R1:8,"), "{t}");
    assert!(t.contains("R1:12"), "{t}");
}

/// `SURED.OR [R4], R6` on a 2D image whose handle is word 0x10 of bank `slot`.
fn sured(slot: u64, op: u64) -> (u64, u64) {
    let lo = 0x59f | APT | 4 << 24 | 6 << 32 | 0x10 << 40 | slot << 54 | 3 << 61;
    (lo, 0x0010_e000 | op << 23)
}

/// `SUATOM.ADD` of size `size` on a 2D image whose handle is word 0x10 of bank `slot`, no result register.
fn suatom(slot: u64, size: u64) -> (u64, u64) {
    let lo = 0x593 | APT | 255 << 16 | 4 << 24 | 6 << 32 | 0x10 << 40 | slot << 54 | 3 << 61;
    (lo, size << 9 | 7 << 17)
}

#[test]
fn suatom_in_the_games_uniform_buffer_names_its_format_by_size() {
    let t = ampere_cs_text(&[suatom(3, 6)]).expect("emit");
    // the size Sd32 gives R32Sint (12) without asking a descriptor
    assert!(t.contains("t=3,12,"), "{t}");
    let t = ampere_cs_text(&[suatom(3, 0)]).expect("emit");
    assert!(t.contains("t=3,11,"), "{t}");
}

#[test]
fn sured_takes_the_operation_of_suatom_and_any_bank() {
    let add = ampere_cs_text(&[sured(3, 0)]).expect("emit");
    let or = ampere_cs_text(&[sured(3, 6)]).expect("emit");
    assert!(add.contains("t=3,11,12582912,"), "{add}");
    assert!(or.contains("t=3,11,12976128,"), "{or}");
    assert!(ampere_cs_text(&[sured(2, 6)]).is_ok());
}

#[test]
fn sured_refuses_another_form() {
    let (lo, hi) = sured(2, 0);
    let r = ampere_cs_text(&[(lo, hi | 1 << 4)]);
    assert!(r.is_err_and(|e| e.contains("SURED form")));
}

/// `ISETP` 0x80d with the immediate `imm`, compare field `cmp` and predicate result `pd`.
fn guard_cull(imm: u64, cmp: u64, pd: u64) -> (u64, u64) {
    (
        0x80d | APT | imm << 32,
        cmp << (76 - 64) | pd << (81 - 64) | 7 << (84 - 64) | 7 << (87 - 64),
    )
}

#[test]
fn guard_cull_never_drops_a_vertex() {
    let t = ampere_vs_text(&[guard_cull(0x8000_0000, 6, 0)]).expect("emit");
    // P0 is written with false, through its shadow
    assert!(
        t.contains("d=R5:0, s=#0,") && t.contains("d=R2:0, s=R5:0,"),
        "{t}"
    );
    assert!(ampere_vs_text(&[guard_cull(0xc432_1111, 0, 7)]).is_ok());
}

#[test]
fn guard_cull_refuses_another_form_and_other_stages() {
    assert!(ampere_vs_text(&[guard_cull(0x1234, 6, 0)]).is_err());
    let r = ampere_cs_text(&[guard_cull(0x8000_0000, 6, 0)]);
    assert!(r.is_err_and(|e| e.contains("guard band cull")));
}

#[test]
fn tmml_takes_a_bindless_handle_in_a_register() {
    let plain = ampere_cs_text(&[tex2d(0xb69)]).expect("emit");
    let ur = ampere_cs_text(&[tex2d(0xf69)]).expect("emit");
    let reg = ampere_cs_text(&[tex2d(0x36a)]).expect("emit");
    assert!(
        !plain.contains("R3:2") && !plain.contains("R1:12"),
        "{plain}"
    );
    assert!(ur.contains("R3:2"), "{ur}");
    assert!(reg.contains("R1:12"), "{reg}");
}

#[test]
fn ldg_reads_its_address_before_the_predicate_zeroes_rd() {
    // LDG.32 R0, [R0.64] @P0 with the false-predicate zeroing of R0
    let ldg = (0x981, 7 | 4 << 9);
    let t = ampere_cs_text(&[ldg]).expect("emit");
    let zero = t.find("d=R5:0, s=#0,").expect(&t);
    let read = t.find("d=l0, s=R5:0,").expect(&t);
    assert!(read < zero, "{t}");
}

#[test]
fn nvn2_compute_c0_maps_separate_textures_and_samplers() {
    // 2 textures, 1 separate texture, 1 separate sampler, no image: c[0] has 0x10 + 8 * 4 bytes
    let layout = r#", "nvn2_cs": [0, 0, 2, 48, 0, 1, 1]"#;
    // the byte offset in c[2] the load is made of
    let at = |offset| {
        let t = ampere_cs_text_with(layout, &[ampere_ldc(0, 255, 0, offset, 0)]).expect("emit");
        t.lines()
            .find(|l| l.starts_with("  op 2 ") && l.contains("s=#0,#"))
            .map(|l| {
                l.rsplit('#')
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(',')
                    .to_string()
            })
            .unwrap_or_default()
    };
    // texture slot 0 at 0x10 is c[2] + 0x20, the separate texture at 0x20 is c[2] + 0x168, the separate sampler at 0x28 is c[2] + 0x568
    assert_eq!(at(0x10), "20");
    assert_eq!(at(0x20), "168");
    assert_eq!(at(0x28), "568");
}

#[test]
fn nvn2_compute_workgroup_counts_read_the_driver_block() {
    let layout = r#", "nvn2_cs": [0, 0, 0, 16, 0]"#;
    // the byte offset in c[0] the load is made of
    let at = |offset| {
        let t = ampere_cs_text_with(layout, &[ampere_ldc(0, 255, 0, offset, 0)]).expect("emit");
        t.lines()
            .find(|l| l.starts_with("  op 2 ") && l.contains("s=#0,#"))
            .map(|l| l.trim_end_matches(',').to_string())
            .unwrap_or_default()
    };
    // c[0] + 4, 8 and 12 are the driver block's dispatch words at 0x200, 0x204 and 0x208
    assert!(at(4).ends_with("s=#0,#200"), "{}", at(4));
    assert!(at(8).ends_with("s=#0,#204"), "{}", at(8));
    assert!(at(12).ends_with("s=#0,#208"), "{}", at(12));
}

#[test]
fn red_f32_add_is_a_compare_and_swap_loop() {
    // RED.E.ADD.F32.FTZ.RN [R0.64], R2
    let red = (0x98e | APT | 2 << 32, 3 << 9);
    let t = ampere_cs_text(&[red, AEXIT]).expect("emit");
    let retry = t.find("op 53 0 0 0 d=L1,").expect(&t);
    let load = t.find("op 4a 11 0 0 d=l0,").expect(&t);
    let add = t.find("op 10002 0 0 0 d=l1, s=l0,").expect(&t);
    let cas = t.find("op 5 11 0 0 d=l2,").expect(&t);
    let back = t.find("op 1a 0 0 0 d=L1, s=l3,").expect(&t);
    assert!(retry < load && load < add && add < cas && cas < back, "{t}");
}

// Encodings from the sm_86 corpus, checked with nvdisasm (low word first)
const R2UR_UR4_R19: (u64, u64) = (0x0000_0000_1304_73c2, 0x000f_e200_000e_0000);

#[test]
fn a_memory_descriptor_is_not_part_of_the_address() {
    // LDG.E R6, desc[UR4][R20.64] against desc[URZ][R20.64]
    let desc = (0x0000_0004_1406_7981, 0x000f_e220_0c1e_1900);
    let urz = (0x0000_003f_1406_7981, 0x000f_e220_0c1e_1900);
    let ops = |ldg| {
        let t = ampere_cs_text(&[R2UR_UR4_R19, ldg]).expect("emit");
        t.lines()
            .filter(|l| !l.starts_with("  cm "))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let a = ops(desc);
    assert_eq!(a, ops(urz));
    assert!(!a.contains("s=R3:4,"), "{a}");
}

#[test]
fn atomg_cas_is_a_compare_and_swap() {
    // ATOMG.E.CAS.STRONG.GPU PT, R6, [R20], R19, R22
    let cas = (0x0000_0013_1406_73a9, 0x000f_e200_001e_e116);
    let t = ampere_cs_text(&[cas]).expect("emit");
    assert_eq!(count(&t, "5"), 1, "{t}");
    assert!(t.contains("op 5 11 0 0 d=l"), "{t}");
    assert!(t.contains("op 35 0 0 0 d=l0, s=R1:19,"), "{t}");
    assert!(t.contains("s=R1:20,R1:21,l0,R1:22,"), "{t}");
}

#[test]
fn atomg_inc_is_a_compare_and_swap_loop() {
    // ATOMG.E.INC.STRONG.GPU PT, R6, [R20], R19
    let inc = (0x0000_0013_1406_73a8, 0x000f_e200_019e_e100);
    let t = ampere_cs_text(&[inc]).expect("emit");
    let retry = t.find("op 53 0 0 0 d=L").expect(&t);
    let load = t.find("op 4a 11 0 0 d=l").expect(&t);
    let cas = t.find("op 5 11 0 0 d=l").expect(&t);
    let back = t.find("op 19 0 0 0 d=L1,").expect(&t);
    assert!(retry < load && load < cas && cas < back, "{t}");
    // DEC of the same form
    let dec = (inc.0, inc.1 & !(0xf << 23) | 4 << 23);
    let t = ampere_cs_text(&[dec]).expect("emit");
    assert!(t.contains("op 5 11 0 0 d=l"), "{t}");
}

#[test]
fn atoms_inc_and_dec_are_a_compare_and_swap_loop() {
    // ATOMS.INC R6, [R19], R20 and ATOMS.DEC R6, [R19], R20
    for hi in [0x000f_e200_0180_0000, 0x000f_e200_0200_0000] {
        let t = ampere_cs_text(&[(0x0000_0014_1306_738c, hi)]).expect("emit");
        let retry = t.find("op 53 0 0 0 d=L1,").expect(&t);
        let load = t.find("op 4a 8 0 0 d=l").expect(&t);
        let cas = t.find("op 5 8 0 0 d=l").expect(&t);
        let back = t.find("op 19 0 0 0 d=L1,").expect(&t);
        assert!(retry < load && load < cas && cas < back, "{t}");
    }
}

#[test]
fn atoms_cas_gives_the_old_value() {
    // ATOMS.CAS R6, [R19], R20, R21 and ATOMS.CAST.SPIN R6, [R19], R20, R21
    let cas = ampere_cs_text(&[(0x0000_0014_1306_738d, 0x000f_e200_0000_0015)]).expect("emit");
    let spin = ampere_cs_text(&[(0x0000_0014_1306_738d, 0x000f_e200_0180_0015)]).expect("emit");
    // The success test: the old value compared with the expected one, then 1 or 0
    let success = |t: &str| t.contains("#1,#0,");
    assert!(cas.contains("op 5 8 0 0 d=l"), "{cas}");
    assert!(!success(&cas), "{cas}");
    assert!(success(&spin), "{spin}");
}

#[test]
fn f2ip_packs_two_saturated_bytes() {
    // F2IP.U8.TRUNC R0, R1, R2
    let f2ip = (0x243 | APT | 1 << 24 | 2 << 32, 0xff | 4 << 8);
    let t = ampere_cs_text(&[f2ip, AEXIT]).expect("emit");
    assert!(t.contains("op 10054 0 0 0 d=l1, s=l0,#0,"), "{t}");
    assert!(t.contains("op 10057 0 0 0 d=l2, s=l1,#437f0000,"), "{t}");
    assert!(t.contains("op 63 0 0 0 d=l8, s=l3,#8,"), "{t}");
    assert!(t.contains("op 17 0 0 0 d=l9, s=l8,l7,"), "{t}");
}

#[test]
fn f2ip_rejects_a_register_in_c() {
    let f2ip = (0x243 | APT | 1 << 24 | 2 << 32, 0x03 | 4 << 8);
    let e = ampere_cs_text(&[f2ip, AEXIT]).expect_err("fail");
    assert!(e.contains("F2IP form"), "{e}");
}

#[test]
fn task_and_mesh_stage_memory_lives_in_shared_memory() {
    let stage = r#", "stage_memory": [0, 16, 32, 64]"#;
    // ISBERD R1, R2 mode 0x78 and 0x38, AST [R2 + 4], R3 in modes 0xb8, 0x80 and 0x88 from RZ
    let isberd = |mode: u64| (0x923 | APT | 1 << 16 | 2 << 24, mode << 8);
    let ast = |mode: u64, rb: u64| (0x927 | APT | 2 << 24 | rb << 32 | 4 << 40, mode << 8);
    let ops = [
        isberd(0x78),
        isberd(0x38),
        ast(0xb8, 3),
        ast(0x80, 3),
        ast(0x88, 255),
        AEXIT,
    ];
    let t = ampere_cs_text_with(stage, &ops).expect("emit");
    // the workgroup number is the constant 0, the payload words are at 0x10 + 4 and 0x20 + 4
    assert!(t.contains("op 35 0 0 0 d=l0, s=#0,"), "{t}");
    assert!(t.contains("s=R1:2,#10,"), "{t}");
    assert!(t.contains("s=R1:2,#24,"), "{t}");
    // a word store, a byte store, and no store for the zero count byte
    assert!(t.contains("op 6c 8 0 0 d= s=#0,l16,R1:3,"), "{t}");
    assert!(t.contains("op 6c 9 0 0 d= s=#0,l21,R1:3,"), "{t}");
    assert_eq!(t.matches("op 6c ").count(), 2, "{t}");
}

#[test]
fn stage_output_store_outside_a_task_or_mesh_shader_is_refused() {
    let ast = (0x927 | APT | 3 << 32, 0xb8 << 8);
    let e = ampere_cs_text(&[ast, AEXIT]).expect_err("fail");
    assert!(e.contains("outside a task or mesh shader"), "{e}");
}

#[test]
fn a_guarded_deferred_write_is_copied_back_only_when_its_guard_held_at_issue() {
    // @P0 LDG.32 R0, [R2.64] with write barrier 0, then EXIT waiting for it
    let ldg = (0x981 | 2 << 24, 4 << 9);
    let exit = (0x94d | APT | 1 << 23, 1 << 52);
    let t = ampere_cs_text(&[ldg, exit]).expect("emit");
    // the guard is kept in a shadow as a nonzero word, taken before the guard's branch
    let keep = t
        .find("op 2a 0 0 0 d=R5:1, s=R2:0,#3f800000,#0,")
        .expect(&t);
    let branch = t.find("op 19 0 0 0 d=L1, s=R2:0,").expect(&t);
    assert!(keep < branch, "{t}");
    // the copy back is skipped when it is zero
    let test = t.find("op 29 0 0 0 d=l3, s=R5:1,#0,").expect(&t);
    let skip = t.find("op 19 0 0 0 d=L2, s=l3,").expect(&t);
    let copy = t.find("op 35 0 0 0 d=R1:0, s=R5:0,").expect(&t);
    let mark = t.find("op 53 0 0 0 d=L2, s=").expect(&t);
    assert!(test < skip && skip < copy && copy < mark, "{t}");
}
