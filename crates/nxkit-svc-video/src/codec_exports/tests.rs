//! Tests of the table of the `nn::codec` exports.

use std::collections::BTreeSet;

use super::*;

#[test]
fn names_follow_the_itanium_scheme_of_the_sdk() {
    let all = table();
    let has = |n: &str| all.iter().any(|e| e.name == n);
    assert!(has("_ZN2nn5codec29CalculateVp9DecoderWorkBufferEPmS1_mtt"));
    assert!(has(
        "_ZN2nn5codec31CalculateDecoderImagePoolBufferEPmS1_RKNS0_27VideoDecoderImagePreferenceEj"
    ));
    assert!(has(
        "_ZN2nn5codec26InitializeDecoderImagePoolEPNS0_25VideoDecoderImagePoolTypeEPvmRKNS0_27VideoDecoderImagePreferenceEjNS0_9CodecModeE"
    ));
    assert!(has(
        "_ZN2nn5codec26ReadDecoderOutputImageRgbaEPKNS0_25VideoDecoderImagePoolTypeEjttttPvmm"
    ));
    assert!(has(
        "_ZN2nn5codec36GetH264DecoderOutputBufferPropertiesEPtS1_S1_S1_PNS_8TimeSpanEPbPNS0_15H264DecoderTypeEj"
    ));
    assert!(has(
        "_ZN2nn5codec21InitializeH264DecoderEPNS0_15H264DecoderTypeEPvmmttjNS0_23VideoDecoderInputFormatENS0_24VideoDecoderOutputFormatENS0_22VideoDecoderOutputModeEPKvm"
    ));
}

#[test]
fn the_codec_video_names_of_the_media_group_are_here() {
    let all = table();
    let names = [
        "_ZN2nn5codec25MakeDecodeImagePreferenceEttNS0_24VideoDecoderOutputFormatENS0_9CodecModeE",
        "_ZN2nn5codec25MakeDecodeImagePreferenceEttNS0_24VideoDecoderOutputFormatENS0_15ImageTilingModeE",
        "_ZN2nn5codec24FinalizeDecoderImagePoolEPNS0_25VideoDecoderImagePoolTypeE",
        "_ZN2nn5codec36GetDecoderImagePoolNativeImageHandleEPKNS0_25VideoDecoderImagePoolTypeEj",
        "_ZN2nn5codec29ReadDecoderOutputImageRgbaHdrEPKNS0_25VideoDecoderImagePoolTypeEjttttPvmm",
        "_ZN2nn5codec26ReadDecoderOutputImageNv12EPKNS0_25VideoDecoderImagePoolTypeEjttttPvmmS4_mm",
        "_ZN2nn5codec37InitializeH264DecoderByTransferMemoryEPNS0_15H264DecoderTypeEPvmmttjNS0_23VideoDecoderInputFormatENS0_24VideoDecoderOutputFormatENS0_22VideoDecoderOutputModeEPKvm",
        "_ZN2nn5codec35InitializeH264DecoderByRemoteMemoryEPNS0_15H264DecoderTypeEmttjNS0_23VideoDecoderInputFormatENS0_24VideoDecoderOutputFormatENS0_22VideoDecoderOutputModeEPKvm",
        "_ZN2nn5codec34GetH264DecoderSubmissionReadyEventEPNS0_15H264DecoderTypeE",
        "_ZN2nn5codec21InitializeH264DecoderEPNS0_15H264DecoderTypeEmmttjNS0_23VideoDecoderInputFormatENS0_24VideoDecoderOutputFormatEPKvm",
        "_ZN2nn5codec36GetH264DecoderOutputBufferPropertiesEPtS1_S1_S1_PNS_5movie10ColorSpaceEPNS_8TimeSpanEPbPNS0_15H264DecoderTypeEj",
    ];
    for n in names {
        assert!(all.iter().any(|e| e.name == n), "{n}");
    }
}

#[test]
fn no_name_is_given_twice() {
    let all = table();
    let names: BTreeSet<&str> = all.iter().map(|e| e.name).collect();
    assert_eq!(names.len(), all.len());
}

#[test]
fn every_name_of_the_nxnative_take_over_is_here() {
    // 4 decoder types of 20 entries, H.264 one more, 10 pool and preference ones, the compat preference,
    // the 2 service ones and 7 sync ones.
    assert_eq!(table().len(), 4 * 20 + 1 + 10 + 1 + 2 + 7);
}
