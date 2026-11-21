#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use crate::spirv::{OP_TYPE_IMAGE, Spv};

fn dims(s: &Spv) -> Vec<u32> {
    s.insts
        .iter()
        .filter(|i| i.op == OP_TYPE_IMAGE)
        .map(|i| i.w[2])
        .collect()
}

#[test]
fn every_1d_image_becomes_2d() {
    let mut s = Spv::parse(crate::tests::IMAGE1D).unwrap();
    assert!(dims(&s).contains(&DIM_1D));
    assert!(as_2d(&mut s).unwrap());
    assert!(dims(&s).iter().all(|&d| d == DIM_2D), "{:?}", dims(&s));
    // no 1D capability is left
    assert!(
        !s.insts
            .iter()
            .any(|i| i.op == OP_CAPABILITY && matches!(i.w[0], CAP_SAMPLED_1D | CAP_IMAGE_1D))
    );
    // the sample takes (x, 0.5): a vec2 is built of the coordinate and the constant 0.5
    let half = s
        .insts
        .iter()
        .find(|i| i.op == OP_CONSTANT && i.w[2] == 0.5f32.to_bits())
        .map(|i| i.w[1])
        .unwrap();
    assert!(
        s.insts
            .iter()
            .any(|i| i.op == OP_COMPOSITE_CONSTRUCT && i.w.get(3) == Some(&half))
    );
    // the size query keeps its scalar result through an extract of the width
    assert!(
        s.insts
            .iter()
            .any(|i| i.op == OP_COMPOSITE_EXTRACT && i.w.get(3) == Some(&0))
    );
    // the module still reads
    crate::spirv::split_combined(&mut s).unwrap();
    crate::to_ir(&s.words(), false, "f").unwrap();
}

#[test]
fn a_module_without_1d_images_is_left_as_it_is() {
    let mut s = Spv::parse(crate::tests::FRAG).unwrap();
    let before = s.words();
    assert!(!as_2d(&mut s).unwrap());
    assert_eq!(s.words(), before);
}

#[test]
fn an_array_of_1d_images_is_an_error() {
    let mut s = Spv::parse(crate::tests::IMAGE1D_ARRAY).unwrap();
    assert!(as_2d(&mut s).unwrap_err().contains("array of 1D images"));
}
