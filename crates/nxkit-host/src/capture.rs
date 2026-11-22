//! The verbs of the live frame capture on the control socket (W36): `capture`, `texture`, `passes`, `stats`, `sample`
//! and `gputrace`. They only parse the request line and call `nxkit_gpu::inspect`, which owns the work.

use std::path::PathBuf;
use std::time::Duration;

use nxkit_gpu::inspect::{self, Request, Want};

use crate::control::Control;

/// How long a verb waits for a frame before it gives up.
const WAIT: Duration = Duration::from_secs(15);

/// Adds the verbs to the control.
pub fn add_verbs(control: &mut Control) {
    control.verb(
        "capture [frame|passes] [DIR]",
        "writes the next frame to <DIR or diag_dir>/capture-<n>/: final.png, manifest.json, with passes a picture of each layer",
        Box::new(|args| {
            let req = parse_capture(args)?;
            inspect::request(req)?.wait(WAIT)
        }),
    );
    control.verb(
        "texture ID [DIR]",
        "writes the texture ID (a TexId) as PNG with its description at the end of the next frame",
        Box::new(|args| {
            let req = parse_texture(args)?;
            inspect::request(req)?.wait(WAIT)
        }),
    );
    control.verb(
        "passes",
        "the passes of the next frame as text lines: index, kind, attachments, draws, programs",
        Box::new(|_| {
            inspect::request(Request {
                want: Want::List,
                dir: None,
            })?
            .wait(WAIT)
        }),
    );
    control.verb(
        "stats",
        "frame time, encode time, GPU time, pipeline builds and textures made, over the last frames",
        Box::new(|_| Ok(inspect::stats_lines())),
    );
    control.verb(
        "sample",
        "makes the next frame a sampled frame: the per-draw NXKIT_DIAG lines print for it",
        Box::new(|_| {
            inspect::sample_next();
            Ok(vec![
                "the next frame is a sampled frame (the lines go to the log, see: log 200)"
                    .to_owned(),
            ])
        }),
    );
    control.verb(
        "gputrace [N] [PATH]",
        "a Metal GPU trace of the next N frames (1) for Xcode, written to PATH or <diag_dir>/frame-<n>-<time>.gputrace",
        Box::new(|args| {
            let (frames, path) = parse_trace(args)?;
            inspect::gpu_trace(frames, path)?.wait(Duration::from_secs(30))
        }),
    );
}

fn dir_of(rest: &str) -> Option<PathBuf> {
    let rest = rest.trim();
    (!rest.is_empty()).then(|| PathBuf::from(rest))
}

/// `[frame|passes] [DIR]`.
pub(crate) fn parse_capture(args: &str) -> Result<Request, String> {
    let args = args.trim();
    let (first, rest) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
    let (want, dir) = match first {
        "frame" => (Want::Frame, rest),
        "passes" => (Want::Passes, rest),
        _ => (Want::Frame, args),
    };
    Ok(Request {
        want,
        dir: dir_of(dir),
    })
}

/// `ID [DIR]`, the id in decimal or as `0x` hex.
pub(crate) fn parse_texture(args: &str) -> Result<Request, String> {
    let args = args.trim();
    let (id, rest) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
    let parsed = match id.strip_prefix("0x") {
        Some(h) => u32::from_str_radix(h, 16),
        None => id.parse(),
    };
    let id = parsed.map_err(|_| "texture needs a TexId (a number): texture ID [DIR]".to_owned())?;
    Ok(Request {
        want: Want::Texture(id),
        dir: dir_of(rest),
    })
}

/// `[N] [PATH]`: the frames (1 to 600) and the document.
pub(crate) fn parse_trace(args: &str) -> Result<(u32, Option<PathBuf>), String> {
    let args = args.trim();
    let (first, rest) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
    match first.parse::<u32>() {
        Ok(n) if (1..=600).contains(&n) => Ok((n, dir_of(rest))),
        Ok(_) => Err("gputrace takes 1 to 600 frames".to_owned()),
        Err(_) => Ok((1, dir_of(args))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_takes_a_mode_and_a_folder() {
        let r = parse_capture("").unwrap();
        assert_eq!((r.want, r.dir), (Want::Frame, None));
        let r = parse_capture("passes").unwrap();
        assert_eq!(r.want, Want::Passes);
        let r = parse_capture("passes /tmp/my dir").unwrap();
        assert_eq!(r.dir, Some(PathBuf::from("/tmp/my dir")));
        let r = parse_capture("/tmp/x").unwrap();
        assert_eq!(
            (r.want, r.dir),
            (Want::Frame, Some(PathBuf::from("/tmp/x")))
        );
        assert_eq!(parse_capture("frame").unwrap().want, Want::Frame);
    }

    #[test]
    fn texture_needs_an_id() {
        assert_eq!(parse_texture("17").unwrap().want, Want::Texture(17));
        assert_eq!(parse_texture("0x1f /tmp").unwrap().want, Want::Texture(31));
        assert!(parse_texture("").is_err());
        assert!(parse_texture("abc").is_err());
    }

    #[test]
    fn gputrace_takes_frames_and_a_path() {
        assert_eq!(parse_trace("").unwrap(), (1, None));
        assert_eq!(parse_trace("3").unwrap(), (3, None));
        assert_eq!(
            parse_trace("2 /tmp/a.gputrace").unwrap(),
            (2, Some(PathBuf::from("/tmp/a.gputrace")))
        );
        assert_eq!(
            parse_trace("/tmp/a.gputrace").unwrap(),
            (1, Some(PathBuf::from("/tmp/a.gputrace")))
        );
        assert!(parse_trace("0").is_err());
        assert!(parse_trace("601").is_err());
    }
}
