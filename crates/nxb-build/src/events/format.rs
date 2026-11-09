//! The text pieces of the events: sizes, the ETA, the live line of the bar, and the clocks.

use std::time::{SystemTime, UNIX_EPOCH};

use super::Cur;

/// `n` bytes as `B`, `KiB`, `MiB` or `GiB`.
pub(super) fn size(n: u64) -> String {
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
pub(super) fn eta(st: &Cur) -> Option<f64> {
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
pub(super) fn live_line(c: &Cur, n: usize, spin: &str, width: usize) -> String {
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

pub(super) fn now() -> f64 {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    (t * 1000.0).round() / 1000.0
}

pub(super) fn clock() -> String {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (_, _, _, h, m, s) = nxb_pack::tz::local(t);
    format!("{h:02}:{m:02}:{s:02}")
}

pub(super) fn hms(s: f64) -> String {
    let s = s as u64;
    let (h, m) = ((s / 60) / 60, (s / 60) % 60);
    if h > 0 {
        format!("{h}:{m:02}:{:02}", s % 60)
    } else {
        format!("{m}:{:02}", s % 60)
    }
}
