//! The NVN function list of a title that gives none: the names in the read-only data of its modules.
//!
//! A game fetches the NVN functions by name through the NVN loader (`nvnBootstrapLoader`, then
//! `nvnDeviceGetProcAddress`), so each name it can ask for is a NUL-terminated string in a module's read-only data.
//! The list is a superset of what one play calls.

/// Whether `s` looks like an NVN C function name: `nvn`, a capital letter, then letters and digits.
fn is_name(s: &[u8]) -> bool {
    s.len() > 4
        && s.len() <= 96
        && s.starts_with(b"nvn")
        && s[3].is_ascii_uppercase()
        && s.iter().all(u8::is_ascii_alphanumeric)
}

/// The NVN function names in `data`, each a whole NUL-terminated string.
fn names_in(data: &[u8], out: &mut std::collections::BTreeSet<String>) {
    let mut start = 0usize;
    for (i, &b) in data.iter().enumerate() {
        if b == 0 {
            let s = &data[start..i];
            if is_name(s) {
                out.insert(String::from_utf8_lossy(s).into_owned());
            }
            start = i + 1;
        }
    }
}

/// The names in the read-only data of the modules, sorted, once each.
pub(crate) fn names(mods: &[(super::modules::Built, u64)]) -> Vec<String> {
    let mut out = std::collections::BTreeSet::new();
    for (m, _) in mods {
        let (off, size) = m.parts[1];
        if let Some(ro) = m.image.get(off..off.saturating_add(size)) {
            names_in(ro, &mut out);
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_strings_that_look_like_nvn_names() {
        let mut out = std::collections::BTreeSet::new();
        names_in(
            b"\0nvnDeviceInitialize\0xnvnA\0nvnfoo\0nvnQueue_Submit\0nvnQueueFinish\0nvn",
            &mut out,
        );
        assert_eq!(
            out.into_iter().collect::<Vec<_>>(),
            ["nvnDeviceInitialize", "nvnQueueFinish"]
        );
    }
}
