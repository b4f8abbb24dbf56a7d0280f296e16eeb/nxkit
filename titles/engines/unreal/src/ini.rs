//! Edits of an ini of the engine: `+CVars=<name>=<value>` lines in the section of a device profile, and `<key>=<value>`
//! lines in sections of the engine configuration.

use nxtitle::{ConfigKey, ConsoleVar};

/// The index of the line after the last line of the section that starts at `at` (the next header, or the end).
fn section_end(lines: &[String], at: usize) -> usize {
    lines[at + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .map_or(lines.len(), |n| at + 1 + n)
}

/// Inserts `new` after the last non-blank line of the section `at..end`.
fn insert_last(lines: &mut Vec<String>, at: usize, end: usize, new: String, eol: &str) {
    let last = (at..end)
        .rev()
        .find(|&k| !lines[k].trim().is_empty())
        .unwrap_or(at);
    if !lines[last].ends_with('\n') {
        lines[last].push_str(eol);
    }
    lines.insert(last + 1, new);
}

fn header_at(lines: &[String], header: &str) -> Option<usize> {
    lines
        .iter()
        .position(|l| l.trim_start_matches('\u{feff}').trim() == header)
}

/// `text` with every key set: a `<key>=` line of its section (without a `+`, `-`, `.` or `!` prefix, which an array
/// key carries) is replaced, otherwise the line is added after the last non-blank line of the section. A missing
/// section is added at the end. The engine reads a later plain `<key>=` line of a section over an earlier one, and this
/// file replaces the game's own, so the value is the one that applies. The line endings of the file are kept.
pub(crate) fn set_keys(text: &str, keys: &[ConfigKey]) -> String {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.split_inclusive('\n').map(String::from).collect();
    for k in keys {
        let header = format!("[{}]", k.section);
        let new = format!("{}={}", k.key, k.value);
        let Some(at) = header_at(&lines, &header) else {
            if let Some(l) = lines.last_mut()
                && !l.ends_with('\n')
            {
                l.push_str(eol);
            }
            lines.push(format!("{header}{eol}"));
            lines.push(format!("{new}{eol}"));
            continue;
        };
        let end = section_end(&lines, at);
        let same = lines[at + 1..end].iter().position(|l| {
            l.trim_start()
                .split_once('=')
                .is_some_and(|(n, _)| n.trim().eq_ignore_ascii_case(k.key))
        });
        if let Some(n) = same {
            let line = &mut lines[at + 1 + n];
            let tail = &line[line.trim_end_matches(['\r', '\n']).len()..];
            *line = format!("{new}{tail}");
            continue;
        }
        insert_last(&mut lines, at, end, format!("{new}{eol}"), eol);
    }
    lines.concat()
}

/// `text` with every variable set in `[section]`: a `+CVars=<name>=...` line of the section is replaced, otherwise a
/// line is added after the last non-blank line of the section. The line endings of the file are kept. Fails when the
/// section is missing.
pub(crate) fn set_cvars(text: &str, section: &str, vars: &[ConsoleVar]) -> Result<String, String> {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.split_inclusive('\n').map(String::from).collect();
    let header = format!("[{section}]");
    let at =
        header_at(&lines, &header).ok_or_else(|| format!("the ini has no section {header}"))?;
    for v in vars {
        let end = section_end(&lines, at);
        let new = format!("+CVars={}={}", v.name, v.value);
        let same = lines[at + 1..end].iter().position(|l| {
            l.trim_start()
                .strip_prefix("+CVars=")
                .and_then(|r| r.split_once('='))
                .is_some_and(|(n, _)| n.trim().eq_ignore_ascii_case(v.name))
        });
        if let Some(n) = same {
            let line = &mut lines[at + 1 + n];
            let tail = &line[line.trim_end_matches(['\r', '\n']).len()..];
            *line = format!("{new}{tail}");
            continue;
        }
        insert_last(&mut lines, at, end, format!("{new}{eol}"), eol);
    }
    Ok(lines.concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: &str = "Switch DeviceProfile";

    fn var(name: &'static str, value: &'static str) -> ConsoleVar {
        ConsoleVar { name, value }
    }

    #[test]
    fn a_line_of_the_section_is_replaced() {
        let t = "[Other]\n+CVars=r.A=9\n[Switch DeviceProfile]\nDeviceType=Switch\n+CVars=r.a=1\n+CVars=r.B=2\n\n[Next]\n";
        let r = set_cvars(t, S, &[var("r.A", "5")]).unwrap();
        assert_eq!(
            r,
            "[Other]\n+CVars=r.A=9\n[Switch DeviceProfile]\nDeviceType=Switch\n+CVars=r.A=5\n+CVars=r.B=2\n\n[Next]\n"
        );
    }

    #[test]
    fn a_line_is_added_after_the_last_line_of_the_section() {
        let t = "[Switch DeviceProfile]\r\nDeviceType=Switch\r\n\r\n[Next]\r\nx=1\r\n";
        let r = set_cvars(t, S, &[var("r.A", "5"), var("r.B", "6")]).unwrap();
        assert_eq!(
            r,
            "[Switch DeviceProfile]\r\nDeviceType=Switch\r\n+CVars=r.A=5\r\n+CVars=r.B=6\r\n\r\n[Next]\r\nx=1\r\n"
        );
    }

    #[test]
    fn the_last_section_without_a_final_newline_gets_one() {
        let r = set_cvars(
            "[Switch DeviceProfile]\nDeviceType=Switch",
            S,
            &[var("r.A", "5")],
        )
        .unwrap();
        assert_eq!(
            r,
            "[Switch DeviceProfile]\nDeviceType=Switch\n+CVars=r.A=5\n"
        );
    }

    #[test]
    fn a_missing_section_is_an_error() {
        let e = set_cvars("[Other]\nx=1\n", S, &[var("r.A", "5")]).unwrap_err();
        assert!(e.contains("[Switch DeviceProfile]"));
    }

    fn key(section: &'static str, key: &'static str, value: &'static str) -> ConfigKey {
        ConfigKey {
            section,
            key,
            value,
        }
    }

    #[test]
    fn a_key_of_a_section_is_replaced_and_an_array_line_is_not() {
        let t = "[/Script/Engine.UserInterfaceSettings]\r\nUIScaleRule=ShortestSide\r\nUIScaleCurve=(old)\r\n+UIScaleCurveX=1\r\n\r\n[Next]\r\n";
        let r = set_keys(
            t,
            &[key(
                "/Script/Engine.UserInterfaceSettings",
                "UIScaleCurve",
                "(new)",
            )],
        );
        assert_eq!(
            r,
            "[/Script/Engine.UserInterfaceSettings]\r\nUIScaleRule=ShortestSide\r\nUIScaleCurve=(new)\r\n+UIScaleCurveX=1\r\n\r\n[Next]\r\n"
        );
    }

    #[test]
    fn a_missing_key_or_section_is_added() {
        let r = set_keys(
            "[A]\nx=1\n\n[B]\ny=2",
            &[key("A", "z", "3"), key("C", "w", "4")],
        );
        assert_eq!(r, "[A]\nx=1\nz=3\n\n[B]\ny=2\n[C]\nw=4\n");
    }
}
