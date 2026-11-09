//! `nn::aoc` take-overs: the add-on content (DLC) list.
//!
//! The owned indices are the numeric sub directories of `Config::aoc_dir` (each one is the RomFS of one DLC, the title
//! id is the base id plus 0x1000 plus the index) or `NAME.img` files. They are fixed at the first query. Without
//! `aoc_dir` no add-on content is owned. A directory and its `.img` replacement report the same DLC once. The list
//! holds every index from 1 to 2000.
//!
//! The engine registers `CountAddOnContent` and `ListAddOnContent` with `HLE_FORCE_BIG`, because the first query lists the
//! directory from a guest thread that can have little stack left. [`big_stack`] gives these two, and
//! [`exports`] gives the others.

use super::oe_state::AppletGuard;
use super::{get_i32, log, put_i32};
use crate::config;
use crate::event;
use nxkit_abi::export::Export;
use std::path::Path;
use std::sync::OnceLock;

const MAX_INDEX: i64 = 2000;

static OWNED: OnceLock<Vec<i32>> = OnceLock::new();

/// The index of a directory entry name: `7` and `7.img` are the index 7, `0`, `07` and `x` are none.
fn index_of(name: &str) -> Option<i32> {
    let digits = name.strip_suffix(".img").unwrap_or(name);
    if !digits.starts_with(|c: char| ('1'..='9').contains(&c))
        || !digits.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    digits
        .parse::<i64>()
        .ok()
        .filter(|&i| i <= MAX_INDEX)
        .map(|i| i as i32)
}

/// The sorted, unique indices in `dir`.
fn scan(dir: &Path) -> Vec<i32> {
    let mut v: Vec<i32> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| index_of(&e.file_name().to_string_lossy()))
                .collect()
        })
        .unwrap_or_default();
    v.sort_unstable();
    v.dedup();
    v
}

fn owned() -> &'static [i32] {
    OWNED.get_or_init(|| {
        let (list, why) = match &config().aoc_dir {
            Some(dir) => (scan(dir), dir.display().to_string()),
            None => (Vec::new(), "no aoc_dir".to_owned()),
        };
        let text: Vec<String> = list.iter().map(i32::to_string).collect();
        log(&format!(
            "aoc: reporting {} add-on contents {{{}}} ({why})",
            list.len(),
            text.join(",")
        ));
        list
    })
}

/// `CountAddOnContent()`.
extern "C" fn count() -> i32 {
    owned().len() as i32
}

/// `ListAddOnContent(int* out, int offset, int count)`: the owned indices from the offset on. Gives the number written.
extern "C" fn list(out: u64, offset: u32, n: u32) -> i32 {
    let all = owned();
    let (offset, n) = (offset as i32, n as i32);
    let mut written = 0;
    for &i in all.iter().skip(offset.max(0) as usize) {
        if written >= n {
            break;
        }
        put_i32(out + 4 * written as u64, i);
        written += 1;
    }
    written
}

/// `GetAddOnContentListChangedEvent(SystemEvent*)`: bound to a kernel event that never fires. A game polls it with
/// `TryWait` and aborts when it is not initialized.
extern "C" fn changed_event(ev: u64) {
    event::bind(ev, false);
}

/// `ShowAddOnContentLostError(const int*, int)`: the error applet, logged only.
extern "C" fn lost_error(indices: u64, n: u32) {
    let _applet = AppletGuard::new("error");
    let shown = if indices != 0 && n > 0 {
        get_i32(indices)
    } else {
        0
    };
    log(&format!(
        "aoc::ShowAddOnContentLostError ({n} entries, the first {shown}) (cut)"
    ));
}

table! {
    "_ZN2nn3aoc31GetAddOnContentListChangedEventEPNS_2os11SystemEventE" => changed_event,
    "_ZN2nn3aoc25ShowAddOnContentLostErrorEPKii" => lost_error,
}

/// The two take-overs that the engine registers with `HLE_FORCE_BIG`.
pub(super) fn big_stack() -> Vec<Export> {
    vec![
        Export::new(
            "_ZN2nn3aoc17CountAddOnContentEv",
            count as *const () as usize,
        ),
        Export::new(
            "_ZN2nn3aoc16ListAddOnContentEPiii",
            list as *const () as usize,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_index_is_a_number_from_1_to_2000() {
        assert_eq!(index_of("1"), Some(1));
        assert_eq!(index_of("2000"), Some(2000));
        assert_eq!(index_of("2001"), None);
        assert_eq!(index_of("0"), None);
        assert_eq!(index_of("07"), None);
        assert_eq!(index_of("x"), None);
        assert_eq!(index_of(""), None);
    }

    #[test]
    fn an_image_has_the_index_of_its_directory() {
        assert_eq!(index_of("12.img"), Some(12));
        assert_eq!(index_of("12.bin"), None);
        assert_eq!(index_of(".img"), None);
    }

    #[test]
    fn the_big_stack_exports_are_the_two_take_overs() {
        let names: Vec<&str> = big_stack().iter().map(|e| e.name).collect();
        assert_eq!(
            names,
            [
                "_ZN2nn3aoc17CountAddOnContentEv",
                "_ZN2nn3aoc16ListAddOnContentEPiii"
            ]
        );
    }
}
