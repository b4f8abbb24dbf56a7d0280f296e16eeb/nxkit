//! The Metal kernels of the network, as one MSL source.
//!
//! The text is kept in `.metal` files next to this module and joined at compile time. Every kernel of
//! [`crate::plan::Plan::steps`] is a `kernel void` of this source; `kernels.rs` names them and sets their grids.
//!
//! - `common.metal`: the shared pieces: the K = 16 step with its f16 rounding, the epilogues and the colour conversion.
//! - `conv.metal`: the twelve convolution layers (3x3 with a staged input tile, 2x2 down, 2x2 up with the skip).
//! - `resample.metal`: the luma pass and the resampler.

/// The MSL of every kernel of [`crate::plan::Plan::steps`].
pub const SOURCE: &str = concat!(
    include_str!("common.metal"),
    include_str!("conv.metal"),
    include_str!("resample.metal"),
);

#[cfg(test)]
mod tests {
    use super::SOURCE;
    use std::io::Write;
    use std::process::{Command, Stdio};

    /// The whole source compiles with the Metal compiler of the SDK. Skipped when `xcrun` or its Metal toolchain is
    /// missing.
    #[test]
    fn source_compiles() {
        let child = Command::new("xcrun")
            .args([
                "-sdk",
                "macosx",
                "metal",
                "-c",
                "-x",
                "metal",
                "-",
                "-o",
                "/dev/null",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let Ok(mut child) = child else {
            eprintln!("skipped: xcrun is not available");
            return;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(SOURCE.as_bytes());
        }
        let Ok(out) = child.wait_with_output() else {
            eprintln!("skipped: xcrun did not finish");
            return;
        };
        let err = String::from_utf8_lossy(&out.stderr);
        if !out.status.success()
            && (err.contains("unable to find utility") || err.contains("cannot execute"))
        {
            eprintln!("skipped: no Metal toolchain: {err}");
            return;
        }
        assert!(out.status.success(), "the MSL does not compile:\n{err}");
    }
}
