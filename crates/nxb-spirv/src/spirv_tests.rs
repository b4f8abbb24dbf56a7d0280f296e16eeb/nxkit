#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;

#[test]
fn recognises_the_magic_number() {
    assert!(is_spirv(&MAGIC.to_le_bytes()));
    assert!(!is_spirv(b"MDB1"));
    assert!(Spv::parse(b"MDB1").is_err());
}

#[test]
fn parse_and_words_round_trip() {
    let bytes = crate::tests::FRAG;
    let s = Spv::parse(bytes).unwrap();
    let back: Vec<u8> = s.words().iter().flat_map(|w| w.to_le_bytes()).collect();
    assert_eq!(back, bytes);
    assert_eq!(s.execution_model().unwrap(), 4);
}

#[test]
fn a_truncated_instruction_is_an_error() {
    let mut bytes = crate::tests::FRAG.to_vec();
    // the first instruction claims more words than the module has
    bytes[22] = 0xff;
    bytes[23] = 0xff;
    assert!(Spv::parse(&bytes).unwrap_err().contains("truncated"));
}

#[test]
fn split_combined_adds_a_sampler_twin() {
    let mut s = Spv::parse(crate::tests::FRAG).unwrap();
    let twins = split_combined(&mut s).unwrap();
    assert_eq!(twins.len(), 1);
    let (&image, &sampler) = twins.iter().next().unwrap();
    assert_eq!(s.decoration(image, DEC_BINDING), Some(2));
    assert_eq!(s.decoration(sampler, DEC_BINDING), Some(2 | TWIN_BINDING));
    assert!(s.insts.iter().any(|i| i.op == OP_SAMPLED_IMAGE));
}

#[test]
fn fix_strides_copies_a_member_stride_onto_the_array_type() {
    // struct { float a[4]; } with ArrayStride 16 on the member and none on the array type
    let mut s = Spv {
        header: [MAGIC, 0x10000, 0, 10, 0],
        insts: Vec::new(),
    };
    s.insts.push(Inst::new(
        OP_MEMBER_DECORATE,
        vec![5, 0, DEC_ARRAY_STRIDE, 16],
    ));
    s.insts.push(Inst::new(OP_TYPE_FLOAT, vec![1, 32]));
    s.insts.push(Inst::new(OP_TYPE_INT, vec![2, 32, 0]));
    s.insts.push(Inst::new(OP_CONSTANT, vec![2, 3, 4]));
    s.insts.push(Inst::new(OP_TYPE_ARRAY, vec![4, 1, 3]));
    s.insts.push(Inst::new(OP_TYPE_STRUCT, vec![5, 4]));
    fix_strides(&mut s);
    assert_eq!(s.decoration(4, DEC_ARRAY_STRIDE), Some(16));
    assert_eq!(s.insts[1].op, OP_DECORATE);
}
