//! The NSP script's console events (`Progress` in `build_ipa_from_nsp.py`): in `--progress json` mode one JSON object
//! per stdout line (`json.dumps(obj, ensure_ascii=False)`), the events `stage`, `step`, `progress`, `stage_end` and
//! `say`, each with `t` (Unix time, 3 decimals) last, the other modes print `[k/n] name` lines. `--progress bar` keeps
//! one live status line at the bottom of a terminal (spinner, stage, bar, count, ETA, elapsed time) that a ticker
//! thread redraws, and prints the permanent lines above it.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nxb_obs::codes::{self, Code};
use nxb_pyjson::Value;

/// How often the plain-lines mode prints the count of a long step, in seconds.
const LINES_INTERVAL: f64 = 30.0;

/// Console mode (`--progress`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// `json`.
    Json,
    /// `lines` / `auto` (plain lines here).
    Lines,
    /// `bar`: plain lines for what stays, and a live status line redrawn in place (a terminal only).
    Bar,
    /// `off`.
    Off,
}

/// What a progress event says apart from the count: step, total, unit, note.
type Shape = (String, Option<u64>, String, String);

struct Cur {
    k: usize,
    name: String,
    t0: Instant,
    /// The step within the stage and its count (`Progress.set` in Python).
    step: String,
    done: Option<u64>,
    total: Option<u64>,
    unit: String,
    note: String,
    /// (when, done) of the last minute, for the ETA.
    hist: std::collections::VecDeque<(f64, u64)>,
    /// The last progress event: when, and what it said apart from the count.
    jlast: Option<(Instant, Shape)>,
    /// When the plain-lines mode last printed the count.
    llast: Option<Instant>,
}

/// A count update for [`Progress::set`] (fields left `None` keep their value).
#[derive(Clone, Debug, Default)]
pub struct Update<'a> {
    /// A new step (restarts the count).
    pub step: Option<&'a str>,
    /// Items done.
    pub done: Option<u64>,
    /// Items in all.
    pub total: Option<u64>,
    /// Unit (`shaders`, `B`).
    pub unit: Option<&'a str>,
    /// Shown after the count.
    pub note: Option<&'a str>,
}

const JSON_INTERVAL: f64 = 0.5;

type State = Mutex<(usize, usize, Option<Cur>)>;
type Out = Mutex<Box<dyn Write + Send>>;

/// The event writer (thread-safe).
pub struct Progress {
    mode: Mode,
    state: Arc<State>,
    out: Arc<Out>,
    bar: Option<Arc<Bar>>,
    ticker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// A count that workers raise with one atomic add per item (or per chunk) and [`Progress::tracked`] reads: no lock and
/// no allocation in the hot loop.
#[derive(Debug, Default)]
pub struct Count {
    /// Items (or bytes) done.
    pub done: AtomicU64,
    /// Items (or bytes) in all, 0 while unknown.
    pub total: AtomicU64,
}

impl Count {
    /// `n` more done.
    pub fn add(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }

    /// The total, once it is known.
    pub fn set_total(&self, n: u64) {
        self.total.store(n, Ordering::Relaxed);
    }

    /// `n` more in all.
    pub fn add_total(&self, n: u64) {
        self.total.fetch_add(n, Ordering::Relaxed);
    }
}

const TICK: Duration = Duration::from_millis(100);
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";

/// The live line: what is drawn, where, and whether the cursor is hidden. Every write holds the `out` lock, and the
/// state lock is held only while the line is formatted.
struct Bar {
    state: Arc<State>,
    out: Arc<Out>,
    t0: Instant,
    stop: AtomicBool,
    /// A live line is on the screen (so the next write clears it first).
    drawn: AtomicBool,
    hidden: AtomicBool,
    columns: Option<usize>,
}

impl Bar {
    fn width(&self) -> usize {
        nxb_sys::stdio::terminal_width(1)
            .or(self.columns)
            .unwrap_or(100)
    }

    /// The live line of the current stage, `None` between stages.
    fn line(&self) -> Option<String> {
        let spin = SPINNER[(self.t0.elapsed().as_millis() / 80) as usize % SPINNER.len()];
        let g = self.state.lock().ok()?;
        let c = g.2.as_ref()?;
        Some(live_line(c, g.1, spin, self.width()))
    }

    /// Clears the live line, writes `perm` as a permanent line, draws the live line again (when `live`, and the bar
    /// has not been stopped). Without a live line the cursor comes back.
    fn emit(&self, perm: Option<&str>, live: bool) {
        let Ok(mut o) = self.out.lock() else { return };
        let line = if live && !self.stop.load(Ordering::Relaxed) {
            self.line()
        } else {
            None
        };
        let mut buf = String::new();
        if self.drawn.load(Ordering::Relaxed) {
            buf.push_str("\r\x1b[2K");
        }
        if let Some(p) = perm {
            buf.push_str(p);
            buf.push('\n');
        }
        match &line {
            Some(l) => {
                if !self.hidden.swap(true, Ordering::Relaxed) {
                    buf.push_str("\x1b[?25l");
                }
                buf.push_str(l);
            }
            None => {
                if self.hidden.swap(false, Ordering::Relaxed) {
                    buf.push_str("\x1b[?25h");
                }
            }
        }
        self.drawn.store(line.is_some(), Ordering::Relaxed);
        if !buf.is_empty() {
            let _ = o.write_all(buf.as_bytes());
            let _ = o.flush();
        }
    }
}

/// `n` bytes as `B`, `KiB`, `MiB` or `GiB`.
fn size(n: u64) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    let (mut v, mut i) = (n as f64, 0);
    while v >= 1024.0 && i < 3 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", ["B", "KiB", "MiB", "GiB"][i])
}

/// The seconds left from the last minute of counts, `None` while there is too little to say.
fn eta(st: &Cur) -> Option<f64> {
    match (st.total, st.hist.front(), st.hist.back(), st.done) {
        (Some(t), Some(a), Some(b), Some(d))
            if st.hist.len() >= 2 && b.0 - a.0 >= 3.0 && b.1 > a.1 =>
        {
            Some(t.saturating_sub(d) as f64 * (b.0 - a.0) / (b.1 - a.1) as f64)
        }
        _ => None,
    }
}

/// `[k/n] name · step`, a bar with a percent when the total is known, the count, the ETA, the stage's running time and
/// the note, cut to `width - 1` columns.
fn live_line(c: &Cur, n: usize, spin: &str, width: usize) -> String {
    let mut s = format!("{spin} [{}/{n}] {}", c.k, c.name);
    if !c.step.is_empty() {
        s.push_str(" · ");
        s.push_str(&c.step);
    }
    let count = |d: u64, t: Option<u64>| {
        let bytes = c.unit == "B";
        let f = |v: u64| if bytes { size(v) } else { v.to_string() };
        let mut x = match t {
            Some(t) => format!("{}/{}", f(d), f(t)),
            None => f(d),
        };
        if !bytes && !c.unit.is_empty() {
            x.push(' ');
            x.push_str(&c.unit);
        }
        x
    };
    match (c.done, c.total) {
        (d, Some(t)) if t > 0 => {
            let d = d.unwrap_or(0);
            let bw = if width >= 100 {
                24
            } else if width >= 70 {
                16
            } else {
                10
            };
            let frac = (d as f64 / t as f64).clamp(0.0, 1.0);
            let fill = (frac * bw as f64) as usize;
            s.push_str(&format!(
                "  [{}{}] {:>3}%  {}",
                "#".repeat(fill),
                "-".repeat(bw - fill),
                (frac * 100.0) as u64,
                count(d, Some(t))
            ));
            if let Some(e) = eta(c) {
                s.push_str(&format!("  ETA {}", hms(e)));
            }
        }
        (Some(d), t) => s.push_str(&format!("  {}", count(d, t.filter(|t| *t > 0)))),
        _ => {}
    }
    s.push_str(&format!("  {}", hms(c.t0.elapsed().as_secs_f64())));
    if !c.note.is_empty() {
        s.push_str("  ");
        s.push_str(&c.note);
    }
    let max = width.saturating_sub(1).max(1);
    if s.chars().count() > max {
        s = s.chars().take(max - 1).collect();
        s.push('…');
    }
    s
}

fn now() -> f64 {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    (t * 1000.0).round() / 1000.0
}

fn clock() -> String {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (_, _, _, h, m, s) = nxb_pack::tz::local(t);
    format!("{h:02}:{m:02}:{s:02}")
}

fn hms(s: f64) -> String {
    let s = s as u64;
    let (h, m) = ((s / 60) / 60, (s / 60) % 60);
    if h > 0 {
        format!("{h}:{m:02}:{:02}", s % 60)
    } else {
        format!("{m}:{:02}", s % 60)
    }
}

impl Progress {
    /// A writer to `out`.
    pub fn new(mode: Mode, out: Box<dyn Write + Send>) -> Progress {
        let state: Arc<State> = Arc::new(Mutex::new((0, 0, None)));
        let out: Arc<Out> = Arc::new(Mutex::new(out));
        let mut p = Progress {
            mode,
            state: state.clone(),
            out: out.clone(),
            bar: None,
            ticker: Mutex::new(None),
        };
        if mode == Mode::Bar {
            let bar = Arc::new(Bar {
                state,
                out,
                t0: Instant::now(),
                stop: AtomicBool::new(false),
                drawn: AtomicBool::new(false),
                hidden: AtomicBool::new(false),
                columns: std::env::var("COLUMNS")
                    .ok()
                    .and_then(|v| v.trim().parse().ok())
                    .filter(|c| *c > 0),
            });
            let b = bar.clone();
            let h = std::thread::Builder::new()
                .name("nxb-bar".into())
                .spawn(move || {
                    while !b.stop.load(Ordering::Relaxed) {
                        std::thread::park_timeout(TICK);
                        if !b.stop.load(Ordering::Relaxed) {
                            b.emit(None, true);
                        }
                    }
                });
            if let Ok(h) = h {
                *p.ticker.lock().unwrap_or_else(|e| e.into_inner()) = Some(h);
            }
            p.bar = Some(bar);
        }
        p
    }

    /// Ends the live line for good: the ticker stops, the line is cleared and the cursor is shown. Safe to call more
    /// than once, and a no-op in the other modes. Later `say` and `end` lines are printed without a live line.
    pub fn finish(&self) {
        let Some(b) = &self.bar else { return };
        b.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.ticker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            h.thread().unpark();
            let _ = h.join();
        }
        b.emit(None, false);
    }

    fn redraw(&self) {
        if let Some(b) = &self.bar {
            b.emit(None, true);
        }
    }

    /// A permanent line: in `bar` mode above the live line, otherwise as it is.
    fn permanent(&self, plain: &str, coloured: &str) {
        match &self.bar {
            Some(b) => b.emit(Some(coloured), true),
            None => self.line(plain),
        }
    }

    /// Runs `f` while a thread reports what `read` returns, `(done, total)`, as the stage's count (every 0.1 s on a
    /// bar, otherwise every 0.25 s, and only when it changed). A total of 0 is unknown: the count shows without a bar.
    /// `step` is set with the count when given (a step that differs from the current one is announced as usual).
    pub fn tracked<T>(
        &self,
        step: Option<&str>,
        unit: &str,
        read: impl Fn() -> (u64, u64) + Sync,
        f: impl FnOnce() -> T,
    ) -> T {
        let stop = AtomicBool::new(false);
        let every = if self.mode == Mode::Bar {
            TICK
        } else {
            Duration::from_millis(250)
        };
        std::thread::scope(|sc| {
            let h = sc.spawn(|| {
                let mut last = None;
                while !stop.load(Ordering::Relaxed) {
                    let (done, total) = read();
                    if (total > 0 || done > 0) && last != Some(done) {
                        last = Some(done);
                        self.set(Update {
                            step,
                            done: Some(done),
                            total: (total > 0).then_some(total),
                            unit: Some(unit),
                            note: None,
                        });
                    }
                    std::thread::park_timeout(every);
                }
            });
            let r = f();
            stop.store(true, Ordering::Relaxed);
            h.thread().unpark();
            r
        })
    }

    /// [`Progress::tracked`] over a [`Count`].
    pub fn counted<T>(
        &self,
        step: Option<&str>,
        unit: &str,
        c: &Count,
        f: impl FnOnce() -> T,
    ) -> T {
        self.tracked(
            step,
            unit,
            || {
                (
                    c.done.load(Ordering::Relaxed),
                    c.total.load(Ordering::Relaxed),
                )
            },
            f,
        )
    }

    /// One event line: the fields, then `code` and `severity` (the code table of `nxb-obs`), then `t`.
    fn json(&self, code: &Code, mut fields: Vec<(String, Value)>) {
        fields.push(("code".into(), Value::Str(code.code.into())));
        fields.push(("severity".into(), Value::Str(code.severity.name().into())));
        fields.push(("t".into(), Value::Float(now())));
        if let Ok(mut o) = self.out.lock() {
            let _ = writeln!(o, "{}", nxb_pyjson::dumps(&Value::Object(fields)));
            let _ = o.flush();
        }
    }

    fn line(&self, s: &str) {
        if let Ok(mut o) = self.out.lock() {
            let _ = writeln!(o, "{s}");
            let _ = o.flush();
        }
    }

    /// `plan(n)`.
    pub fn plan(&self, n: usize) {
        if let Ok(mut g) = self.state.lock() {
            (g.0, g.1) = (0, n);
        }
    }

    /// `plan_more(n)`: `n` more stages after those begun so far (the numbering goes on).
    pub fn plan_more(&self, n: usize) {
        if let Ok(mut g) = self.state.lock() {
            g.1 = g.0 + n;
        }
    }

    /// `stage(name, step)`: ends the current stage, starts the next.
    pub fn stage(&self, name: &str, step: &str) {
        self.end("", true);
        let (k, n) = match self.state.lock() {
            Ok(mut g) => {
                g.0 += 1;
                g.2 = Some(Cur {
                    k: g.0,
                    name: name.into(),
                    t0: Instant::now(),
                    step: step.into(),
                    done: None,
                    total: None,
                    unit: String::new(),
                    note: String::new(),
                    hist: Default::default(),
                    jlast: None,
                    llast: None,
                });
                (g.0, g.1)
            }
            Err(_) => return,
        };
        match self.mode {
            Mode::Json => self.json(
                &codes::STAGE,
                vec![
                    ("ev".into(), Value::Str("stage".into())),
                    ("k".into(), Value::Int(k as i64)),
                    ("n".into(), Value::Int(n as i64)),
                    ("name".into(), Value::Str(name.into())),
                    ("step".into(), Value::Str(step.into())),
                ],
            ),
            Mode::Bar => self.redraw(),
            Mode::Lines => self.line(&format!(
                "[{}] [{k}/{n}] {name}{} …",
                clock(),
                if step.is_empty() {
                    String::new()
                } else {
                    format!(" · {step}")
                }
            )),
            Mode::Off => self.line(&format!("[{}] [{k}/{n}] {name}", clock())),
        }
    }

    /// `end(summary, ok)`.
    pub fn end(&self, summary: &str, ok: bool) {
        self.end_with(summary, ok, None);
    }

    /// `end(summary, ok)` with the stage's own running time when it ran beside others (`None`: since its `stage` event).
    pub fn end_with(&self, summary: &str, ok: bool, took: Option<f64>) {
        let cur = match self.state.lock() {
            Ok(mut g) => g.2.take(),
            Err(_) => None,
        };
        let Some(c) = cur else { return };
        let el = took.unwrap_or_else(|| c.t0.elapsed().as_secs_f64());
        match self.mode {
            Mode::Json => self.json(
                if ok {
                    &codes::STAGE_END_OK
                } else {
                    &codes::STAGE_END_FAIL
                },
                vec![
                    ("ev".into(), Value::Str("stage_end".into())),
                    ("k".into(), Value::Int(c.k as i64)),
                    ("name".into(), Value::Str(c.name)),
                    ("ok".into(), Value::Bool(ok)),
                    ("summary".into(), Value::Str(summary.into())),
                    (
                        "elapsed".into(),
                        Value::Float((el * 1000.0).round() / 1000.0),
                    ),
                ],
            ),
            _ => {
                let n = self.state.lock().map(|g| g.1).unwrap_or(0);
                let tail = format!(
                    "[{}/{n}] {} ({}){}",
                    c.k,
                    c.name,
                    hms(el),
                    if summary.is_empty() {
                        String::new()
                    } else {
                        format!(": {summary}")
                    }
                );
                self.permanent(
                    &format!("[{}] {} {tail}", clock(), if ok { "✓" } else { "✗" }),
                    &format!(
                        "{DIM}[{}]{RESET} {} {tail}",
                        clock(),
                        if ok {
                            format!("{GREEN}✓{RESET}")
                        } else {
                            format!("{RED}✗{RESET}")
                        }
                    ),
                )
            }
        }
    }

    /// `set(...)`: a `step` event when the step changes, then a `progress` event when anything changed (a count change
    /// alone at most every 0.5 s), with Python's fields: `k name step done total unit est note frac eta elapsed t`.
    pub fn set(&self, u: Update<'_>) {
        let Ok(mut g) = self.state.lock() else { return };
        let n_all = g.1;
        let Some(st) = g.2.as_mut() else { return };
        let mut events: Vec<(&Code, Vec<(String, Value)>)> = Vec::new();
        let mut lines = Vec::new();
        let mut redraw = false;
        if let Some(step) = u.step
            && step != st.step
        {
            st.step = step.into();
            (st.done, st.total, st.unit, st.note) = (None, None, String::new(), String::new());
            st.hist.clear();
            events.push((
                &codes::STEP,
                vec![
                    ("ev".into(), Value::Str("step".into())),
                    ("k".into(), Value::Int(st.k as i64)),
                    ("name".into(), Value::Str(st.name.clone())),
                    ("step".into(), Value::Str(step.into())),
                ],
            ));
            redraw = true;
            if self.mode == Mode::Lines {
                lines.push(format!(
                    "[{}] [{}/{n_all}] {} · {step} …",
                    clock(),
                    st.k,
                    st.name
                ));
            }
        }
        if let Some(x) = u.unit {
            st.unit = x.into();
        }
        if let Some(x) = u.total {
            st.total = Some(x);
        }
        if let Some(x) = u.note {
            st.note = x.into();
        }
        let wall = now();
        if let Some(d) = u.done {
            st.done = Some(d);
            st.hist.push_back((wall, d));
            while st.hist.len() > 2 && st.hist[1].0 < wall - 60.0 {
                st.hist.pop_front();
            }
        }
        let shape = (self.mode == Mode::Json)
            .then(|| (st.step.clone(), st.total, st.unit.clone(), st.note.clone()));
        let send = match (&st.jlast, &shape) {
            (_, None) => false,
            (None, _) => true,
            (Some((t, prev)), Some(shape)) => {
                prev != shape || t.elapsed().as_secs_f64() >= JSON_INTERVAL
            }
        };
        if let (true, Some(shape)) = (send, shape) {
            st.jlast = Some((Instant::now(), shape));
            let opt = |v: Option<u64>| v.map_or(Value::Null, |x| Value::Int(x as i64));
            let frac = match (st.done, st.total) {
                (Some(d), Some(t)) if t > 0 => Value::Float((d as f64 / t as f64).min(0.99)),
                _ => Value::Null,
            };
            let eta = eta(st).map_or(Value::Null, Value::Float);
            let el = st.t0.elapsed().as_secs_f64();
            events.push((
                &codes::PROGRESS,
                vec![
                    ("ev".into(), Value::Str("progress".into())),
                    ("k".into(), Value::Int(st.k as i64)),
                    ("name".into(), Value::Str(st.name.clone())),
                    ("step".into(), Value::Str(st.step.clone())),
                    ("done".into(), opt(st.done)),
                    ("total".into(), opt(st.total)),
                    ("unit".into(), Value::Str(st.unit.clone())),
                    ("est".into(), Value::Bool(false)),
                    ("note".into(), Value::Str(st.note.clone())),
                    ("frac".into(), frac),
                    ("eta".into(), eta),
                    (
                        "elapsed".into(),
                        Value::Float((el * 1000.0).round() / 1000.0),
                    ),
                ],
            ));
        }
        // Plain lines show the count too, at most every LINES_INTERVAL, so a log (a build piped into a file) says how
        // far a long step is, with an ETA when there is one.
        if self.mode == Mode::Lines
            && let Some(d) = st.done
            && st
                .llast
                .is_none_or(|t| t.elapsed().as_secs_f64() >= LINES_INTERVAL)
        {
            st.llast = Some(Instant::now());
            let of = st.total.map_or(String::new(), |t| format!(" of {t}"));
            let pct = match st.total {
                Some(t) if t > 0 => format!(" ({:.0}%)", 100.0 * d as f64 / t as f64),
                _ => String::new(),
            };
            let left = eta(st).map_or(String::new(), |e| {
                format!(", about {:.0} min left", (e / 60.0).ceil())
            });
            lines.push(format!(
                "[{}] [{}/{n_all}] {} · {}: {d}{of} {}{pct}{left}",
                clock(),
                st.k,
                st.name,
                st.step,
                st.unit
            ));
        }
        drop(g);
        match self.mode {
            Mode::Json => events.into_iter().for_each(|(c, e)| self.json(c, e)),
            Mode::Lines => lines.iter().for_each(|l| self.line(l)),
            Mode::Bar if redraw => self.redraw(),
            Mode::Bar | Mode::Off => {}
        }
    }

    /// `say(msg)`.
    pub fn say(&self, msg: &str) {
        self.say_with(msg, Vec::new());
    }

    /// `say(msg)` with additional fields on the JSON event (after `msg`, before `t`).
    pub fn say_with(&self, msg: &str, extra: Vec<(String, Value)>) {
        match self.mode {
            Mode::Json => {
                let mut f = vec![
                    ("ev".into(), Value::Str("say".into())),
                    ("msg".into(), Value::Str(msg.into())),
                ];
                f.extend(extra);
                self.json(codes::say_code(msg), f);
            }
            _ => self.permanent(
                &format!("[{}] {msg}", clock()),
                &format!("{DIM}[{}]{RESET} {msg}", clock()),
            ),
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Schema: every line is one JSON object with `ev`, `code`, `severity`, `t` last, and the pair is a row of the table.
    #[test]
    fn every_event_has_a_table_code_and_old_fields_stay() {
        let buf = Buf::default();
        let p = Progress::new(Mode::Json, Box::new(buf.clone()));
        p.plan(2);
        p.stage("extract", "first");
        p.set(Update {
            step: Some("second"),
            done: Some(1),
            total: Some(4),
            unit: Some("files"),
            note: None,
        });
        p.say("plain");
        p.say("[RED] broken");
        p.say("[YELLOW] careful");
        p.end("done", true);
        p.stage("pack", "");
        p.end("boom", false);
        let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        let mut seen = Vec::new();
        for line in text.lines() {
            let v = nxb_pyjson::parse(line).unwrap();
            let Value::Object(fields) = &v else {
                panic!("{line}")
            };
            let ev = v.get("ev").and_then(Value::as_str).unwrap();
            let code = v.get("code").and_then(Value::as_str).unwrap();
            let sev = v.get("severity").and_then(Value::as_str).unwrap();
            let row = nxb_obs::codes::lookup(code).unwrap();
            assert_eq!((row.ev, row.severity.name()), (ev, sev), "{line}");
            assert_eq!(fields.last().unwrap().0, "t");
            seen.push((ev.to_string(), code.to_string()));
        }
        let want = |ev: &str, code: &str| {
            assert!(
                seen.contains(&(ev.into(), code.into())),
                "{ev} {code} in {seen:?}"
            )
        };
        want("stage", "NXB1001");
        want("step", "NXB1004");
        want("progress", "NXB1005");
        want("say", "NXB1006");
        want("say", "NXB1007");
        want("say", "NXB1008");
        want("stage_end", "NXB1002");
        want("stage_end", "NXB1003");
    }
}
