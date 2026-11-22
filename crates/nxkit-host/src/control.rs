//! The control socket of a running game (W35): a person or an agent asks the host questions and changes the live
//! settings while the game stays in its scene.
//!
//! The host listens on a Unix socket `$TMPDIR/nxkit/<pid>.sock` (only the user can open `$TMPDIR` on macOS).
//! `NXKIT_CONTROL=0` turns it off. `tools/nxkit-ctl` is the client.
//!
//! One request is one line. The answer is zero or more lines and then one status line, `.ok` or `.err <why>`. A line
//! of the answer that starts with `.` gets one more `.` in front, so no line of the answer looks like a status line.
//!
//! The built-in verbs are `help`, `keys`, `get`, `set`, `unset`, `info` and `log`. Other parts of the host add their
//! own verbs ([`Control::verb`]), for example the frame capture of the encoder. The control knows only the board of
//! the live keys and its verbs, and no part knows the socket.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use nxkit_abi::live::Board;

/// What a verb answers: the lines of the answer, or why it failed.
pub type Answer = Result<Vec<String>, String>;

/// A verb that a part of the host adds. It gets the rest of the request line after the verb.
pub type Handler = Box<dyn FnMut(&str) -> Answer + Send>;

struct Verb {
    usage: &'static str,
    help: &'static str,
    handler: Handler,
}

/// The board of the live keys, the facts of the run and the verbs of the parts.
pub struct Control {
    board: Board,
    info: Vec<(String, String)>,
    verbs: BTreeMap<&'static str, Verb>,
}

/// The built-in verbs, for `help`.
const BUILT_IN: &[(&str, &str)] = &[
    ("help", "the verbs"),
    (
        "keys",
        "the live keys with their values now and what they do",
    ),
    ("get KEY", "the value of a live key now"),
    ("set KEY=VALUE", "changes a live key while the game runs"),
    ("unset KEY", "removes a live key, so its default applies"),
    ("info", "the facts of the run: pid, game, folders"),
    (
        "log [N | +CURSOR]",
        "the last N lines of the log (100), or the lines after CURSOR, then cursor=<next>",
    ),
];

impl Control {
    /// A control over `board`. `info` are the facts that `info` prints.
    pub fn new(board: Board, info: Vec<(String, String)>) -> Self {
        Control {
            board,
            info,
            verbs: BTreeMap::new(),
        }
    }

    /// Adds a verb. `usage` starts with the name of the verb (`capture [passes]`).
    pub fn verb(&mut self, usage: &'static str, help: &'static str, handler: Handler) {
        let name = usage.split_whitespace().next().unwrap_or(usage);
        self.verbs.insert(
            name,
            Verb {
                usage,
                help,
                handler,
            },
        );
    }

    /// Answers one request line.
    pub fn answer(&mut self, line: &str) -> Answer {
        let line = line.trim();
        let (verb, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        match verb {
            "" => Ok(Vec::new()),
            "help" => Ok(self.help()),
            "keys" => Ok(self
                .board
                .rows()
                .into_iter()
                .map(|r| {
                    format!(
                        "{}={}  # {} ({})",
                        r.name,
                        r.value.unwrap_or_default(),
                        r.help,
                        r.owners.join(", ")
                    )
                })
                .collect()),
            "get" => match self.board.value(rest) {
                Some(v) => Ok(vec![format!("{rest}={v}")]),
                None => Ok(vec![format!("{rest} is not set")]),
            },
            "set" => {
                let (k, v) = rest
                    .split_once('=')
                    .or_else(|| rest.split_once(char::is_whitespace))
                    .ok_or("set needs KEY=VALUE")?;
                let (k, v) = (k.trim(), v.trim());
                let owners = self.board.set(k, Some(v)).map_err(|e| e.to_string())?;
                Ok(vec![format!("{k}={v}  # applied in {}", owners.join(", "))])
            }
            "unset" => {
                let owners = self.board.set(rest, None).map_err(|e| e.to_string())?;
                Ok(vec![format!(
                    "{rest} unset  # applied in {}",
                    owners.join(", ")
                )])
            }
            "info" => Ok(self.info.iter().map(|(k, v)| format!("{k}={v}")).collect()),
            "log" => log(rest),
            other => match self.verbs.get_mut(other) {
                Some(v) => (v.handler)(rest),
                None => Err(format!("unknown verb {other:?} (help lists the verbs)")),
            },
        }
    }

    fn help(&self) -> Vec<String> {
        let built = BUILT_IN.iter().map(|(u, h)| (*u, *h));
        let added = self.verbs.values().map(|v| (v.usage, v.help));
        built
            .chain(added)
            .map(|(u, h)| format!("{u:<28} {h}"))
            .collect()
    }
}

/// The `log` verb over the tail of the log pipe.
fn log(rest: &str) -> Answer {
    let read = |t: &crate::logtail::Tail| match rest.strip_prefix('+') {
        Some(c) => c
            .trim()
            .parse::<u64>()
            .map(|c| t.since(c))
            .map_err(|_| "log +CURSOR needs a number".to_string()),
        None if rest.is_empty() => Ok(t.last(100)),
        None => rest
            .parse::<usize>()
            .map(|n| t.last(n))
            .map_err(|_| "log needs N or +CURSOR".to_string()),
    };
    let lines = tail(read).ok_or("the log does not go through the pipe")??;
    let mut out = Vec::with_capacity(lines.lines.len() + 2);
    if lines.gap {
        out.push("# the log dropped lines before these".to_string());
    }
    out.extend(lines.lines);
    out.push(format!("cursor={}", lines.cursor));
    Ok(out)
}

#[cfg(target_os = "macos")]
fn tail<R>(read: impl FnOnce(&crate::logtail::Tail) -> R) -> Option<R> {
    crate::sys::logpipe::tail(read)
}

#[cfg(not(target_os = "macos"))]
fn tail<R>(_read: impl FnOnce(&crate::logtail::Tail) -> R) -> Option<R> {
    None
}

/// The folder of the sockets: `$TMPDIR/nxkit`.
pub fn folder() -> PathBuf {
    std::env::temp_dir().join("nxkit")
}

/// Starts the control on a thread of its own, on `<folder>/<pid>.sock`. Returns the path of the socket. A failure is
/// not fatal for the game: the host prints it and runs without the control.
pub fn start(mut control: Control) -> std::io::Result<PathBuf> {
    let dir = folder();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.sock", std::process::id()));
    // A socket of an earlier process with the same pid is stale.
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    control
        .info
        .push(("socket".into(), path.display().to_string()));
    std::thread::Builder::new()
        .name("nxkit-control".into())
        .spawn(move || serve(&listener, &mut control))?;
    Ok(path)
}

/// Answers one client after the other. A client keeps its connection for as many requests as it wants.
fn serve(listener: &UnixListener, control: &mut Control) {
    for stream in listener.incoming().flatten() {
        if let Err(e) = session(stream, control) {
            eprintln!("nxkit-host: control: {e}");
        }
    }
}

fn session(stream: UnixStream, control: &mut Control) -> std::io::Result<()> {
    let mut out = stream.try_clone()?;
    for line in BufReader::new(stream).lines() {
        let line = line?;
        let answer = control.answer(&line);
        out.write_all(frame(&answer).as_bytes())?;
        out.flush()?;
    }
    Ok(())
}

/// The bytes of an answer on the socket (see the module).
fn frame(answer: &Answer) -> String {
    let mut s = String::new();
    let (lines, status) = match answer {
        Ok(lines) => (lines.as_slice(), ".ok".to_string()),
        Err(why) => (&[][..], format!(".err {}", why.replace('\n', " "))),
    };
    for l in lines {
        for part in l.split('\n') {
            if part.starts_with('.') {
                s.push('.');
            }
            s.push_str(part);
            s.push('\n');
        }
    }
    s.push_str(&status);
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_abi::env::Env;
    use nxkit_abi::live::{LiveKey, Section};

    const KEYS: &[LiveKey] = &[LiveKey {
        name: "NXKIT_CTL_TEST",
        help: "a test key",
    }];

    fn control() -> Control {
        let section = Section {
            owner: "test",
            keys: KEYS,
            apply: |_| {},
        };
        let board = Board::new(Env::default(), vec![section]);
        Control::new(board, vec![("pid".into(), "1".into())])
    }

    #[test]
    fn the_built_in_verbs_answer() {
        let mut c = control();
        assert_eq!(
            c.answer("set NXKIT_CTL_TEST=on"),
            Ok(vec!["NXKIT_CTL_TEST=on  # applied in test".to_string()])
        );
        assert_eq!(
            c.answer("get NXKIT_CTL_TEST"),
            Ok(vec!["NXKIT_CTL_TEST=on".to_string()])
        );
        assert!(
            c.answer("keys")
                .is_ok_and(|l| l[0].starts_with("NXKIT_CTL_TEST=on"))
        );
        assert!(c.answer("unset NXKIT_CTL_TEST").is_ok());
        assert_eq!(
            c.answer("get NXKIT_CTL_TEST"),
            Ok(vec!["NXKIT_CTL_TEST is not set".to_string()])
        );
        assert!(
            c.answer("set NXKIT_PRIVATE=1")
                .is_err_and(|e| e.contains("start only"))
        );
        assert_eq!(c.answer("info"), Ok(vec!["pid=1".to_string()]));
        assert!(c.answer("nope").is_err());
    }

    #[test]
    fn a_part_adds_a_verb() {
        let mut c = control();
        c.verb(
            "echo TEXT",
            "says TEXT again",
            Box::new(|rest| Ok(vec![rest.to_string()])),
        );
        assert_eq!(c.answer("echo hi there"), Ok(vec!["hi there".to_string()]));
        assert!(
            c.answer("help")
                .is_ok_and(|l| l.iter().any(|h| h.starts_with("echo TEXT")))
        );
    }

    #[test]
    fn an_answer_line_never_looks_like_a_status_line() {
        assert_eq!(
            frame(&Ok(vec![".ok".into(), "a\nb".into()])),
            "..ok\na\nb\n.ok\n"
        );
        assert_eq!(frame(&Err("no\nway".into())), ".err no way\n");
    }

    #[test]
    fn a_client_talks_to_the_socket() {
        let dir = std::env::temp_dir().join(format!("nxkit-ctl-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("t.sock");
        let _ = std::fs::remove_file(&path);
        let Ok(listener) = UnixListener::bind(&path) else {
            return;
        };
        let mut c = control();
        let server = std::thread::spawn(move || {
            if let Some(Ok(s)) = listener.incoming().next() {
                let _ = session(s, &mut c);
            }
        });
        let Ok(mut client) = UnixStream::connect(&path) else {
            return;
        };
        let _ = client.write_all(b"info\n");
        let _ = client.shutdown(std::net::Shutdown::Write);
        let mut got = String::new();
        let _ = std::io::Read::read_to_string(&mut client, &mut got);
        let _ = server.join();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(got, "pid=1\n.ok\n");
    }
}
