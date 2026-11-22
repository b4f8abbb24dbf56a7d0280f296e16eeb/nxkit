//! The `shader` verb of the control socket (W36): the hot swap of the shaders of a running game.
//!
//! The verb is a thin parser over `nxkit_gpu::ShaderHub`. The hub does the work, and its docs say how. The control
//! thread blocks while the hub counts frames (`list`) or compiles (`load`), so a request waits for its own answer.

use nxkit_gpu::ShaderHub;

use crate::control::{Answer, Control};

/// One request of the `shader` verb.
#[derive(Debug, PartialEq, Eq)]
enum Request {
    /// The programs of the last frames, filtered by a part of the key.
    List(String),
    Dump(String),
    Load(String),
    /// A key or `all`.
    Reset(String),
    Status,
}

const USAGE: &str = "shader list [FILTER] | dump KEY | load KEY | reset KEY|all | status";

/// Reads the text after `shader`.
fn parse(rest: &str) -> Result<Request, String> {
    let rest = rest.trim();
    let (word, arg) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let arg = arg.trim();
    let key = || {
        if arg.is_empty() || arg.contains(char::is_whitespace) {
            Err(format!("shader {word} needs one KEY ({USAGE})"))
        } else {
            Ok(arg.to_string())
        }
    };
    match word {
        "list" => Ok(Request::List(arg.to_string())),
        "dump" => key().map(Request::Dump),
        "load" => key().map(Request::Load),
        "reset" => key().map(Request::Reset),
        "status" if arg.is_empty() => Ok(Request::Status),
        "" => Err(format!("shader needs a word: {USAGE}")),
        other => Err(format!("unknown shader word {other:?}: {USAGE}")),
    }
}

/// Runs a request on the hub.
fn run(hub: &ShaderHub, rest: &str) -> Answer {
    match parse(rest)? {
        Request::List(filter) => hub.list(&filter),
        Request::Dump(key) => hub.dump(&key),
        Request::Load(key) => hub.load(&key),
        Request::Reset(what) => hub.reset(&what),
        Request::Status => {
            let mut out = vec![format!("override folder {}", hub.dir().display())];
            let swapped = hub.swapped();
            out.push(format!("{} swapped", swapped.len()));
            out.extend(swapped);
            Ok(out)
        }
    }
}

/// Adds the `shader` verb to the control.
pub(crate) fn shader_verbs(control: &mut Control, hub: &ShaderHub) {
    let hub = hub.clone();
    control.verb(
        "shader list|dump|load|reset|status",
        "swaps the MSL of a program (see docs/diagnostics.md, Shader hot swap). To skip one: set NXKIT_SKIP_PROGRAMS=KEY",
        Box::new(move |rest| run(&hub, rest)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_words_are_parsed() {
        assert_eq!(parse("list"), Ok(Request::List(String::new())));
        assert_eq!(parse(" list  0ab "), Ok(Request::List("0ab".into())));
        assert_eq!(parse("dump 0abc"), Ok(Request::Dump("0abc".into())));
        assert_eq!(parse("load 0abc"), Ok(Request::Load("0abc".into())));
        assert_eq!(parse("reset all"), Ok(Request::Reset("all".into())));
        assert_eq!(parse("status"), Ok(Request::Status));
    }

    #[test]
    fn a_bad_request_says_what_is_wrong() {
        for bad in ["", "dump", "load a b", "reset", "status now", "skip 0abc"] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        assert!(parse("skip 0abc").is_err_and(|e| e.contains("unknown")));
        assert!(parse("dump").is_err_and(|e| e.contains("needs one KEY")));
    }

    #[test]
    fn the_verb_answers_through_the_control() {
        use nxkit_abi::env::Env;
        let hub = ShaderHub::new(std::env::temp_dir().join("nxkit-shaderctl-test"));
        let mut c = Control::new(nxkit_abi::live::Board::new(Env::default(), vec![]), vec![]);
        shader_verbs(&mut c, &hub);
        let status = c.answer("shader status").unwrap();
        assert!(status[0].starts_with("override folder") && status[1] == "0 swapped");
        // the GPU has not attached: the verbs say so
        assert!(c.answer("shader load 0abc").is_err());
        assert!(
            c.answer("shader dump 0abc")
                .is_err_and(|e| e.contains("not started"))
        );
        assert!(
            c.answer("help")
                .is_ok_and(|l| l.iter().any(|h| h.starts_with("shader ")))
        );
    }
}
