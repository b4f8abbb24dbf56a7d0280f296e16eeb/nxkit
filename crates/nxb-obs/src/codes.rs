//! The code table: every event kind has a stable `code` and a `severity` (design 2.14).
//!
//! Codes are `NXB` plus four digits and never change meaning once released. The table is the only place that lists them.
//! `1xxx` is the front ends' stream (`stage`, `stage_end`, `step`, `progress`, `say`), `2xxx` the library events of this
//! crate, `3xxx` is reserved for the cache, `4xxx` for the manifest and partial state.

/// How serious an event is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    /// Detail for debugging.
    Debug,
    /// Normal progress.
    Info,
    /// Something degraded, the run goes on.
    Warn,
    /// Something failed.
    Error,
}

impl Severity {
    /// The name in the stream.
    pub fn name(self) -> &'static str {
        match self {
            Severity::Debug => "debug",
            Severity::Info => "info",
            Severity::Warn => "warn",
            Severity::Error => "error",
        }
    }
}

/// One row of the table.
#[derive(Debug, PartialEq, Eq)]
pub struct Code {
    /// The stable code.
    pub code: &'static str,
    /// The `ev` tag it goes with.
    pub ev: &'static str,
    /// The severity.
    pub severity: Severity,
    /// What it says, for humans.
    pub meaning: &'static str,
}

const fn c(
    code: &'static str,
    ev: &'static str,
    severity: Severity,
    meaning: &'static str,
) -> Code {
    Code {
        code,
        ev,
        severity,
        meaning,
    }
}

/// A stage starts.
pub const STAGE: Code = c("NXB1001", "stage", Severity::Info, "a stage starts");
/// A stage ends well.
pub const STAGE_END_OK: Code = c("NXB1002", "stage_end", Severity::Info, "a stage ends well");
/// A stage ends with a failure.
pub const STAGE_END_FAIL: Code = c("NXB1003", "stage_end", Severity::Error, "a stage fails");
/// A step starts within a stage.
pub const STEP: Code = c("NXB1004", "step", Severity::Info, "a step starts");
/// A count update.
pub const PROGRESS: Code = c("NXB1005", "progress", Severity::Debug, "a count update");
/// A message.
pub const SAY: Code = c("NXB1006", "say", Severity::Info, "a message");
/// A message that starts with `[RED]` or `nxb: error:`.
pub const SAY_RED: Code = c("NXB1007", "say", Severity::Error, "an error message");
/// A message that starts with `[YELLOW]`, or whose first words are `<program>: warning:` (`nxb: warning:`,
/// `nxkit-rt: warning:`).
pub const SAY_WARN: Code = c("NXB1008", "say", Severity::Warn, "a warning message");
/// A phase starts.
pub const PHASE: Code = c("NXB2001", "phase", Severity::Info, "a phase starts");
/// Library progress.
pub const LIB_PROGRESS: Code = c("NXB2002", "progress", Severity::Debug, "a count update");
/// A shader finished well.
pub const SHADER_OK: Code = c("NXB2003", "shader", Severity::Debug, "a shader is done");
/// A shader failed.
pub const SHADER_FAIL: Code = c("NXB2004", "shader", Severity::Warn, "a shader failed");
/// An error.
pub const ERROR: Code = c("NXB2005", "error", Severity::Error, "an error with a class");
/// The summary of a run.
pub const SUMMARY: Code = c("NXB2006", "summary", Severity::Info, "the summary of a run");

/// Every row, in code order.
pub const TABLE: &[&Code] = &[
    &STAGE,
    &STAGE_END_OK,
    &STAGE_END_FAIL,
    &STEP,
    &PROGRESS,
    &SAY,
    &SAY_RED,
    &SAY_WARN,
    &PHASE,
    &LIB_PROGRESS,
    &SHADER_OK,
    &SHADER_FAIL,
    &ERROR,
    &SUMMARY,
];

/// Whether `msg` starts with `<program>: warning:`, the shape of the warnings of `nxb` and the nxkit crates.
fn is_tagged_warning(msg: &str) -> bool {
    msg.split_once(": warning:").is_some_and(|(program, _)| {
        !program.is_empty()
            && program
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

/// The code of a `say` message, from its prefix.
pub fn say_code(msg: &str) -> &'static Code {
    if msg.starts_with("[RED]") || msg.starts_with("nxb: error:") {
        &SAY_RED
    } else if msg.starts_with("[YELLOW]") || is_tagged_warning(msg) {
        &SAY_WARN
    } else {
        &SAY
    }
}

/// The row for a code string.
pub fn lookup(code: &str) -> Option<&'static Code> {
    TABLE.iter().copied().find(|r| r.code == code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_unique_and_well_formed() {
        for w in TABLE.windows(2) {
            assert!(w[0].code < w[1].code, "{} before {}", w[0].code, w[1].code);
        }
        for r in TABLE {
            assert!(r.code.starts_with("NXB") && r.code.len() == 7);
            assert!(r.code[3..].bytes().all(|b| b.is_ascii_digit()));
            assert_eq!(lookup(r.code), Some(*r));
        }
    }

    #[test]
    fn say_codes_by_prefix() {
        assert_eq!(say_code("[RED] x"), &SAY_RED);
        assert_eq!(say_code("nxb: error: x"), &SAY_RED);
        assert_eq!(say_code("[YELLOW] x"), &SAY_WARN);
        assert_eq!(say_code("nxb: warning: x"), &SAY_WARN);
        assert_eq!(say_code("nxkit-rt: warning: x"), &SAY_WARN);
        assert_eq!(say_code("a b: warning: x"), &SAY);
        assert_eq!(say_code("hello"), &SAY);
    }
}
