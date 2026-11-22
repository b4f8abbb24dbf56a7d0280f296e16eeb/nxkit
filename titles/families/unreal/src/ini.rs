//! Edits of a device profile ini: `+CVars=<name>=<value>` lines in one section.

use nxtitle::ConsoleVar;

/// `text` with every variable set in `[section]`: a `+CVars=<name>=...` line of the section is replaced, otherwise a
/// line is added after the last non-blank line of the section. The line endings of the file are kept. Fails when the
/// section is missing.
pub(crate) fn set_cvars(text: &str, section: &str, vars: &[ConsoleVar]) -> Result<String, String> {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.split_inclusive('\n').map(String::from).collect();
    let header = format!("[{section}]");
    let at = lines
        .iter()
        .position(|l| l.trim_start_matches('\u{feff}').trim() == header)
        .ok_or_else(|| format!("the ini has no section {header}"))?;
    for v in vars {
        let end = lines[at + 1..]
            .iter()
            .position(|l| l.trim_start().starts_with('['))
            .map_or(lines.len(), |n| at + 1 + n);
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
        let last = (at..end)
            .rev()
            .find(|&k| !lines[k].trim().is_empty())
            .unwrap_or(at);
        if !lines[last].ends_with('\n') {
            lines[last].push_str(eol);
        }
        lines.insert(last + 1, format!("{new}{eol}"));
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
}
