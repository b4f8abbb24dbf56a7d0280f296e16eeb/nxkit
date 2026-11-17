//! The anonymous guest region reserved by dyld as part of the signed library.
//! Its file-backed __LINKEDIT follows it and must never enter the GPU buffer.
use crate::{Game, HostError, Result, layout, macho};

fn bad(message: &str) -> HostError {
    HostError::Memory(format!("iOS guest tail: {message}"))
}

/// The relocated game and the usable part of its dyld-owned reservation.
#[derive(Debug)]
pub struct Plan {
    /// Manifest relocated by the verified dyld slide.
    pub game: Game,
    /// Guest address-space layout after relocation.
    pub layout: layout::Layout,
    /// Anonymous tail owned by dyld, excluding the file-backed link data.
    pub reserved: layout::Range,
}

/// Validate the converter contract, then plan the runtime within __NX_HEAP.
/// A zero slide checks the file before dlopen; the actual dyld slide is checked
/// again before any guest address is registered or handed to Metal.
pub fn plan(game: &Game, segments: &[macho::Segment], slide: i64) -> Result<Plan> {
    if slide.rem_euclid(layout::PAGE as i64) != 0 {
        return Err(bad("dyld slide is not page aligned"));
    }
    let unique = |name: &str| -> Result<(usize, &macho::Segment)> {
        let mut matches = segments.iter().enumerate().filter(|(_, s)| s.name == name);
        let found = matches
            .next()
            .ok_or_else(|| bad(&format!("missing {name}; reconvert the game")))?;
        if matches.next().is_some() {
            return Err(bad(&format!("duplicate {name}")));
        }
        Ok(found)
    };
    let (tail_index, tail) = unique("__NX_HEAP")?;
    let (link_index, link) = unique("__LINKEDIT")?;
    if tail.vmsize == 0
        || !tail.vmaddr.is_multiple_of(layout::PAGE)
        || !tail.vmsize.is_multiple_of(layout::PAGE)
        || tail.filesize != 0
        || tail.initprot != 3
        || tail.fileoff != link.fileoff
        || link.initprot != 1
        || link.vmsize == 0
        || link.filesize > link.vmsize
        || tail_index + 1 != link_index
        || link_index + 1 != segments.len()
    {
        return Err(bad(
            "expected a page-aligned anonymous RW segment before __LINKEDIT",
        ));
    }
    let preferred_end = tail
        .vmaddr
        .checked_add(tail.vmsize)
        .ok_or_else(|| bad("segment overflow"))?;
    if link.vmaddr != preferred_end {
        return Err(bad("__LINKEDIT does not follow the reservation"));
    }
    // Validate every slid segment, including file-backed __LINKEDIT, even though
    // only module segments and the anonymous tail belong to the guest range.
    for segment in segments {
        let preferred_end = segment
            .vmaddr
            .checked_add(segment.vmsize)
            .ok_or_else(|| bad("segment overflow"))?;
        segment
            .vmaddr
            .checked_add_signed(slide)
            .and_then(|start| start.checked_add(segment.vmsize))
            .ok_or_else(|| bad("slid segment overflow"))?;
        if segment.name != "__NX_HEAP"
            && segment.name != "__LINKEDIT"
            && preferred_end > tail.vmaddr
        {
            return Err(bad("another segment overlaps or follows the reservation"));
        }
    }
    let start = tail
        .vmaddr
        .checked_add_signed(slide)
        .ok_or_else(|| bad("slid reservation overflow"))?;
    let end = start
        .checked_add(tail.vmsize)
        .ok_or_else(|| bad("slid reservation overflow"))?;
    let moved = game.moved(slide)?;
    for module in &moved.modules {
        let end = module
            .base
            .checked_add_signed(module.low)
            .and_then(|start| start.checked_add(module.span))
            .ok_or_else(|| bad("slid module overflow"))?;
        if end > start {
            return Err(bad("a module overlaps the reservation"));
        }
    }
    let layout = layout::plan(&moved, start)?;
    if layout.tls.start != start || layout.all.end() > end {
        return Err(bad(
            "reservation is too small for the manifest; reconvert the game",
        ));
    }
    Ok(Plan {
        game: moved,
        layout,
        reserved: layout::Range {
            start,
            len: tail.vmsize,
        },
    })
}

/// Confirm every byte of the used tail is anonymous and read/write, never
/// executable or file-backed. The callback reports start, length, protection
/// and external-pager status from the process VM map. No mapping is changed.
#[cfg(any(target_os = "ios", test))]
pub(crate) fn inspect(
    range: layout::Range,
    mut region: impl FnMut(u64) -> Option<(u64, u64, u32, bool)>,
) -> Result<()> {
    let end = range
        .start
        .checked_add(range.len)
        .ok_or_else(|| bad("VM range overflow"))?;
    if range.len == 0
        || !range.start.is_multiple_of(layout::PAGE)
        || !range.len.is_multiple_of(layout::PAGE)
    {
        return Err(bad("invalid VM range"));
    }
    let mut at = range.start;
    while at < end {
        let (start, len, protection, external_pager) =
            region(at).ok_or_else(|| bad("VM region is missing"))?;
        let stop = start
            .checked_add(len)
            .ok_or_else(|| bad("VM region overflow"))?;
        if start > at || stop <= at || protection != 3 || external_pager {
            return Err(bad("VM region is not anonymous read/write memory"));
        }
        at = stop.min(end);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    fn fixture() -> (Game, Vec<macho::Segment>) {
        let game = Game::parse(
            Path::new("/x"),
            &crate::manifest::sample("", crate::module::samples::NEAR),
        )
        .unwrap();
        let mut segments: Vec<_> = game.modules[0]
            .segments
            .iter()
            .map(|s| macho::Segment {
                name: s.name.clone(),
                vmaddr: game.modules[0].segment_addr(s),
                vmsize: s.size,
                fileoff: 0x4000,
                filesize: s.file_size.next_multiple_of(layout::PAGE),
                initprot: if s.prot == crate::module::Prot::Exec {
                    5
                } else {
                    3
                },
            })
            .collect();
        let start = game.modules[0].end();
        // Deliberately more than the 4 GiB sample needs, independent of phase.
        let len = 4 << 30;
        segments.push(macho::Segment {
            name: "__NX_HEAP".into(),
            vmaddr: start,
            vmsize: len,
            fileoff: 0x10000,
            filesize: 0,
            initprot: 3,
        });
        segments.push(macho::Segment {
            name: "__LINKEDIT".into(),
            vmaddr: start + len,
            vmsize: 0x4000,
            fileoff: 0x10000,
            filesize: 12,
            initprot: 1,
        });
        (game, segments)
    }
    #[test]
    fn every_slide_phase_keeps_the_gpu_range_in_the_tail() {
        let (game, segments) = fixture();
        for delta in [-0x8000_0000i64, 0, 0x7000_0000] {
            for phase in (0..layout::REGION_ALIGN).step_by(layout::PAGE as usize) {
                let p = plan(&game, &segments, delta + phase as i64).unwrap();
                assert_eq!(p.layout.tls.start, p.reserved.start);
                assert!(p.layout.all.end() <= p.reserved.end());
                assert_eq!(
                    p.game.modules[0].base,
                    game.modules[0]
                        .base
                        .checked_add_signed(delta + phase as i64)
                        .unwrap()
                );
            }
        }
    }
    #[test]
    fn signing_growth_changes_no_guest_address() {
        let (game, mut segments) = fixture();
        let before = plan(&game, &segments, 0).unwrap().layout;
        let link = segments.last_mut().unwrap();
        link.filesize += 0x9000;
        link.vmsize += 0x10000;
        assert_eq!(plan(&game, &segments, 0).unwrap().layout, before);
    }
    #[test]
    fn rejects_missing_duplicate_malformed_and_small_reservations() {
        let (game, segments) = fixture();
        let t = segments.len() - 2;
        for case in 0..12 {
            let mut s = segments.clone();
            match case {
                0 => {
                    s.remove(t);
                }
                1 => s.insert(t, s[t].clone()),
                2 => s[t].vmaddr += 1,
                3 => s[t].vmsize = 0,
                4 => s[t].filesize = 0x4000,
                5 => s[t].initprot = 7,
                6 => {
                    s[t].vmsize = 0x4000;
                    s[t + 1].vmaddr = s[t].vmaddr + 0x4000;
                }
                7 => s[t + 1].vmaddr += 0x4000,
                8 => s[t + 1].initprot = 3,
                9 => s[0].vmsize = u64::MAX,
                10 => s[t + 1].vmsize = u64::MAX,
                _ => s[t].fileoff += 0x4000,
            }
            assert!(plan(&game, &s, 0).is_err(), "case {case}");
        }
        assert!(plan(&game, &segments, 1).is_err());
        assert!(plan(&game, &segments, -0x2_0000_0000).is_err());
        let mut large = game.clone();
        large.memory.heap = 8 << 30;
        assert!(plan(&large, &segments, 0).is_err());
        let mut overlap = game.clone();
        overlap.modules[0].span += layout::PAGE;
        assert!(plan(&overlap, &segments, 0).is_err());
    }
    #[test]
    fn vm_inspection_accepts_split_and_larger_anonymous_regions() {
        let range = layout::Range {
            start: 0x4000,
            len: 0x10000,
        };
        let mut calls = 0;
        inspect(range, |at| {
            calls += 1;
            Some((at, 0x4000, 3, false))
        })
        .unwrap();
        assert_eq!(calls, 4);
        inspect(range, |_| Some((0, 0x20000, 3, false))).unwrap();
    }
    #[test]
    fn vm_inspection_rejects_holes_file_pages_execute_and_overflow() {
        let range = layout::Range {
            start: 0x4000,
            len: 0x10000,
        };
        for report in [
            None,
            Some((0x8000, 0x10000, 3, false)),
            Some((0, 0x4000, 3, false)),
            Some((0, 0x20000, 5, false)),
            Some((0, 0x20000, 7, false)),
            Some((0, 0x20000, 3, true)),
            Some((0x4000, u64::MAX, 3, false)),
        ] {
            assert!(inspect(range, |_| report).is_err());
        }
        assert!(
            inspect(
                layout::Range {
                    start: u64::MAX,
                    len: 1
                },
                |_| unreachable!()
            )
            .is_err()
        );
    }
}
