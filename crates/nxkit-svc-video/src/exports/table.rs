//! The table of exports that this service gives the game.

use super::*;

/// The functions that this service gives the game.
pub(crate) fn table() -> Vec<Export> {
    vec![
        Export::new(
            "_ZN5movie7DecoderC1ENS_11DecoderTypeENS_19DecoderOutputFormatENS_11DecoderModeEi",
            ctor_full as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7DecoderC2ENS_11DecoderTypeENS_19DecoderOutputFormatENS_11DecoderModeEi",
            ctor_full as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7DecoderC1ENS_11DecoderTypeENS_19DecoderOutputFormatENS_11DecoderModeE",
            ctor_no_priority as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7DecoderC2ENS_11DecoderTypeENS_19DecoderOutputFormatENS_11DecoderModeE",
            ctor_no_priority as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7DecoderC1ENS_11DecoderTypeENS_19DecoderOutputFormatE",
            ctor_sdk3 as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7DecoderC2ENS_11DecoderTypeENS_19DecoderOutputFormatE",
            ctor_sdk3 as *const () as usize,
        ),
        Export::new("_ZN5movie7DecoderD0Ev", dtor_delete as *const () as usize),
        Export::new("_ZN5movie7DecoderD1Ev", dtor as *const () as usize),
        Export::new("_ZN5movie7DecoderD2Ev", dtor as *const () as usize),
        Export::new(
            "_ZN5movie7Decoder9ConfigureEPNS_9MediaDataE",
            configure as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder20RegisterClientEventsEPNS_13DecoderEventsE",
            register_client_events as *const () as usize,
        ),
        Export::new("_ZN5movie7Decoder5StartEv", start as *const () as usize),
        Export::new("_ZN5movie7Decoder4StopEv", stop as *const () as usize),
        Export::new("_ZN5movie7Decoder5ResetEv", reset as *const () as usize),
        Export::new("_ZN5movie7Decoder5FlushEv", flush as *const () as usize),
        Export::new(
            "_ZN5movie7Decoder23AcquireInputBufferIndexEPiS1_",
            acquire_input_buffer_index as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder14GetInputBufferEiPNS_6BufferE",
            get_input_buffer as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder24SendInputBufferForDecodeEiiilj",
            send_input_buffer_for_decode as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder24AcquireOutputBufferIndexEPiPlPjS1_",
            acquire_output_buffer_index as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder15GetOutputFormatEPNS_9MediaDataE",
            get_output_format as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder21GetOutputBufferFormatEiPNS_9MediaDataE",
            get_output_buffer_format as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder19GetOutputBufferSizeEPiS1_",
            get_output_buffer_size as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder15GetOutputBufferEiPNS_6BufferE",
            get_output_buffer as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder15GetOutputBufferEiPNS_6BufferEPNS_9MediaDataE",
            get_output_buffer_with_format as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder24ReleaseOutputBufferIndexEi",
            release_output_buffer_index as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder12GetLastErrorEv",
            get_last_error as *const () as usize,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_has_each_name_once() {
        let t = table();
        let mut names: Vec<&str> = t.iter().map(|e| e.name).collect();
        names.sort_unstable();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n);
        assert!(
            t.iter()
                .all(|e| e.addr != 0 && e.name.starts_with("_ZN5movie7Decoder"))
        );
    }
}
