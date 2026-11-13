//! Reserve the anonymous guest tail in dyld's initial image allocation.
//! File contents and all existing module addresses remain unchanged.
use nxb_pyjson::Value as J;
type Result<T> = std::result::Result<T, String>;
fn field<'a>(j: &'a J, key: &str) -> Result<&'a J> {
    j.get(key).ok_or_else(|| format!("missing {key}"))
}
const PAGE: u64 = 0x4000;
const ALIGN: u64 = 0x20_0000;
const MARKER: usize = 0x3ffc;
const SEGMENT: usize = 72;

fn bad() -> String {
    "iOS guest tail size or address overflows".into()
}
fn align(n: u64, a: u64) -> Result<u64> {
    n.checked_add(a - 1).map(|n| n & !(a - 1)).ok_or_else(bad)
}
fn size(memory: &J, key: &str) -> Result<u64> {
    let n = match field(memory, key)? {
        J::Int(n) => u64::try_from(*n).ok(),
        J::Str(s) => s
            .strip_prefix("0x")
            .map_or_else(|| s.parse().ok(), |s| u64::from_str_radix(s, 16).ok()),
        _ => None,
    };
    n.filter(|n| *n < 1 << 39)
        .ok_or_else(|| format!("invalid memory.{key} for iOS guest tail"))
}

/// Upper bound valid for every 16 KiB dyld slide, including negative slides.
/// Runtime layout rounds the absolute heap address up to 2 MiB, so reserve a
/// full extra alignment unit rather than assuming the preferred-address phase.
pub(crate) fn length(game: &J, main_stack: u64) -> Result<u64> {
    let memory = field(game, "memory")?;
    let (reserve, alias, stack) = (
        size(memory, "reserve")?,
        size(memory, "alias")?,
        size(memory, "stack")?,
    );
    let heap = reserve
        .checked_sub(alias)
        .and_then(|n| n.checked_sub(stack))
        .and_then(|n| n.checked_sub(256 << 20))
        .ok_or_else(bad)?;
    if heap < ALIGN {
        return Err("iOS guest tail needs at least 2 MiB heap".into());
    }
    let main = align(main_stack, PAGE)?.checked_add(PAGE).ok_or_else(bad)?;
    let stack = align(stack.checked_add(main).ok_or_else(bad)?, ALIGN)?;
    let mut total = (16 << 20) + (64 << 20) + ALIGN;
    for n in [align(heap, ALIGN)?, stack, align(alias, ALIGN)?] {
        total = total.checked_add(n).ok_or_else(bad)?;
    }
    if total >= 1 << 39 {
        return Err(bad());
    }
    Ok(total)
}
fn word(b: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        b.get(at..at + 4)
            .ok_or("truncated iOS Mach-O")?
            .try_into()
            .map_err(|_| "invalid integer width")?,
    ))
}
fn wide(b: &[u8], at: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(
        b.get(at..at + 8)
            .ok_or("truncated iOS segment")?
            .try_into()
            .map_err(|_| "invalid integer width")?,
    ))
}

/// Called after ios_dylib adds its signing marker. Insert one LC_SEGMENT_64
/// before __LINKEDIT, whose VM address moves but whose file position does not.
pub(crate) fn install(b: &mut [u8], game: &J, main_stack: u64) -> Result<()> {
    let len = length(game, main_stack)?;
    let count = word(b, 16)?;
    let end = 32usize.checked_add(word(b, 20)? as usize).ok_or_else(bad)?;
    // Leave at least LC_CODE_SIGNATURE (16 bytes) before the existing __text marker.
    if b.len() < 0x4000
        || end.checked_add(SEGMENT + 16).is_none_or(|n| n > MARKER)
        || b[end..end + SEGMENT].iter().any(|v| *v != 0)
    {
        return Err("no load-command room for iOS guest tail and signing".into());
    }
    let mut link = None;
    let mut at = 32;
    let mut image_end = 0;
    for _ in 0..count {
        let cmd = word(b, at)?;
        let n = word(b, at + 4)? as usize;
        let next = at.checked_add(n).ok_or_else(bad)?;
        if n < 8 || !n.is_multiple_of(8) || next > end {
            return Err("invalid iOS load command".into());
        }
        // Inserting a segment changes segment indices; this adapter does not
        // rewrite chained-fixup starts tables. Refuse future writer changes.
        if cmd == 0x80000034 {
            return Err("iOS guest tail does not support LC_DYLD_CHAINED_FIXUPS".into());
        }
        if cmd == 0x19 {
            if n < SEGMENT {
                return Err("short iOS segment".into());
            }
            let name = &b[at + 8..at + 24];
            let name = &name[..name.iter().position(|v| *v == 0).unwrap_or(name.len())];
            if name == b"__NX_HEAP" {
                return Err("duplicate iOS guest tail".into());
            }
            let start = wide(b, at + 24)?;
            let stop = start.checked_add(wide(b, at + 32)?).ok_or_else(bad)?;
            if name == b"__LINKEDIT" {
                if link.replace((at, start, wide(b, at + 40)?)).is_some() {
                    return Err("duplicate __LINKEDIT".into());
                }
            } else {
                image_end = image_end.max(stop);
            }
        }
        at = next;
    }
    if at != end {
        return Err("inconsistent iOS load commands".into());
    }
    let (at, start, fileoff) = link.ok_or("missing __LINKEDIT for iOS guest tail")?;
    if start != image_end || !start.is_multiple_of(PAGE) {
        return Err("__LINKEDIT does not follow iOS modules".into());
    }
    let new_link = start.checked_add(len).ok_or_else(bad)?;
    new_link.checked_add(wide(b, at + 32)?).ok_or_else(bad)?;
    let new_count = count.checked_add(1).ok_or_else(bad)?;
    let new_size = u32::try_from(end - 32 + SEGMENT).map_err(|_| bad())?;
    // All fallible checks precede mutation.
    b.copy_within(at..end, at + SEGMENT);
    b[at..at + SEGMENT].fill(0);
    b[at..at + 4].copy_from_slice(&0x19u32.to_le_bytes());
    b[at + 4..at + 8].copy_from_slice(&(SEGMENT as u32).to_le_bytes());
    b[at + 8..at + 17].copy_from_slice(b"__NX_HEAP");
    for (off, value) in [(24, start), (32, len), (40, fileoff), (48, 0)] {
        b[at + off..at + off + 8].copy_from_slice(&value.to_le_bytes());
    }
    b[at + 56..at + 60].copy_from_slice(&3u32.to_le_bytes());
    b[at + 60..at + 64].copy_from_slice(&3u32.to_le_bytes());
    b[at + SEGMENT + 24..at + SEGMENT + 32].copy_from_slice(&new_link.to_le_bytes());
    b[16..20].copy_from_slice(&new_count.to_le_bytes());
    b[20..24].copy_from_slice(&new_size.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn game() -> J {
        nxb_pyjson::parse(
            r#"{"memory":{"reserve":4294967296,"alias":268435456,"stack":268435456}}"#,
        )
        .unwrap()
    }
    fn image() -> Vec<u8> {
        let mut b = vec![0; 0x8000];
        for (at, n) in [
            (16, 2u32),
            (20, 144),
            (32, 0x19),
            (36, 72),
            (104, 0x19),
            (108, 72),
        ] {
            b[at..at + 4].copy_from_slice(&n.to_le_bytes());
        }
        b[40..46].copy_from_slice(b"__TEXT");
        b[112..122].copy_from_slice(b"__LINKEDIT");
        for (at, n) in [
            (56, 0x100_0000_0000u64),
            (64, PAGE),
            (80, PAGE),
            (128, 0x100_0000_4000),
            (136, PAGE),
            (144, PAGE),
            (152, 8),
        ] {
            b[at..at + 8].copy_from_slice(&n.to_le_bytes());
        }
        b[MARKER..MARKER + 4].copy_from_slice(&0xd65f03c0u32.to_le_bytes());
        b[0x4000..0x4008].copy_from_slice(b"linkedit");
        b
    }
    #[test]
    fn matches_layout_for_every_slide_phase() {
        let len = length(&game(), 16 << 20).unwrap();
        for phase in (0..ALIGN).step_by(PAGE as usize) {
            let start = (0x100_0000_0000u64)
                .checked_add_signed(-0xff_0000_0000i64 + phase as i64)
                .unwrap();
            let heap = align(start + (16 << 20) + (64 << 20), ALIGN).unwrap();
            let end = heap
                + 0xd0000000
                + align((256 << 20) + (16 << 20) + PAGE, ALIGN).unwrap()
                + (256 << 20);
            assert!(end <= start + len);
        }
    }
    #[test]
    fn respects_merged_overrides_and_hex() {
        let mut g = game();
        let before = length(&g, 0x100000).unwrap();
        let mut memory = field(&g, "memory").unwrap().clone();
        memory.set("reserve", J::str("0x80000000"));
        g.set("memory", memory);
        assert_eq!(length(&g, 0x100000).unwrap(), before - (2 << 30));
    }
    #[test]
    fn preserves_module_and_file_positions() {
        let mut b = image();
        let old = b.clone();
        let len = length(&game(), 0x100000).unwrap();
        install(&mut b, &game(), 0x100000).unwrap();
        assert_eq!(&b[32..104], &old[32..104]);
        assert_eq!(&b[0x4000..], &old[0x4000..]);
        assert_eq!(&b[MARKER..MARKER + 4], &old[MARKER..MARKER + 4]);
        assert_eq!(wide(&b, 104 + 24).unwrap(), 0x100_0000_4000);
        assert_eq!(wide(&b, 104 + 32).unwrap(), len);
        assert_eq!(wide(&b, 176 + 24).unwrap(), 0x100_0000_4000 + len);
        assert_eq!(&b[176 + 40..176 + 72], &old[104 + 40..104 + 72]);
        assert_eq!(word(&b, 16).unwrap(), 3);
        let end = 32 + word(&b, 20).unwrap() as usize;
        assert!(end + 16 <= MARKER);
        assert!(b[end..end + 16].iter().all(|v| *v == 0));
    }
    #[test]
    fn requires_room_for_the_signing_command() {
        for (end, works) in [(16288usize, true), (16296, false)] {
            let mut b = image();
            b[16..20].copy_from_slice(&3u32.to_le_bytes());
            b[20..24].copy_from_slice(&((end - 32) as u32).to_le_bytes());
            b[176..180].copy_from_slice(&0x777u32.to_le_bytes());
            b[180..184].copy_from_slice(&((end - 176) as u32).to_le_bytes());
            let before = b.clone();
            assert_eq!(install(&mut b, &game(), 1).is_ok(), works);
            if !works {
                assert_eq!(b, before);
            }
        }
    }
    #[test]
    fn segment_names_end_at_first_nul() {
        // Match dyld/the host: bytes after the first NUL are name padding.
        let mut padded_link = image();
        padded_link[123] = 0x7f;
        install(&mut padded_link, &game(), 1).unwrap();

        let mut duplicate_heap = image();
        install(&mut duplicate_heap, &game(), 1).unwrap();
        duplicate_heap[123] = 0x7f;
        let before = duplicate_heap.clone();
        assert!(
            install(&mut duplicate_heap, &game(), 1)
                .unwrap_err()
                .contains("duplicate iOS guest tail")
        );
        assert_eq!(duplicate_heap, before);

        let mut duplicate_link = image();
        duplicate_link[40..56].fill(0);
        duplicate_link[40..50].copy_from_slice(b"__LINKEDIT");
        duplicate_link[51] = 0x7f;
        let before = duplicate_link.clone();
        assert!(
            install(&mut duplicate_link, &game(), 1)
                .unwrap_err()
                .contains("duplicate __LINKEDIT")
        );
        assert_eq!(duplicate_link, before);
    }
    #[test]
    fn rejects_chained_fixups_without_mutation() {
        let mut b = image();
        b[16..20].copy_from_slice(&3u32.to_le_bytes());
        b[20..24].copy_from_slice(&160u32.to_le_bytes());
        b[176..180].copy_from_slice(&0x80000034u32.to_le_bytes());
        b[180..184].copy_from_slice(&16u32.to_le_bytes());
        let before = b.clone();
        assert!(
            install(&mut b, &game(), 1)
                .unwrap_err()
                .contains("LC_DYLD_CHAINED_FIXUPS")
        );
        assert_eq!(b, before);
    }
    #[test]
    fn rejects_overflow_and_malformed_without_mutation() {
        assert!(length(&game(), u64::MAX).is_err());
        for raw in [
            r#"{"memory":{"reserve":"0xffffffffffffffff","alias":0,"stack":0}}"#,
            r#"{"memory":{"reserve":1,"alias":2,"stack":3}}"#,
        ] {
            assert!(length(&nxb_pyjson::parse(raw).unwrap(), 1).is_err());
        }
        for at in [20usize, 128] {
            let mut b = image();
            b[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            let before = b.clone();
            assert!(install(&mut b, &game(), 1).is_err());
            assert_eq!(b, before);
        }
        let mut b = image();
        install(&mut b, &game(), 1).unwrap();
        let before = b.clone();
        assert!(install(&mut b, &game(), 1).is_err());
        assert_eq!(b, before);
    }
}
