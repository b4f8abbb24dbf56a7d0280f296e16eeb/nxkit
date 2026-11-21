//! The defined dynamic symbols of a module image. The observation (`obs`), the regions and the reader (`rw`) use them.

use std::collections::HashMap;

/// `{name: (offset, size)}` of the module's defined dynamic symbols, empty without a symbol table it can read.
pub(crate) fn defined(img: &[u8]) -> HashMap<String, (i64, i64)> {
    fn go(img: &[u8]) -> Option<HashMap<String, (i64, i64)>> {
        let u32at = |o: i64| -> Option<i64> {
            let o = usize::try_from(o).ok()?;
            img.get(o..o + 4)
                .map(|s| i64::from(u32::from_le_bytes([s[0], s[1], s[2], s[3]])))
        };
        let q = |o: i64| -> Option<(i64, u64)> {
            let o = usize::try_from(o).ok()?;
            let s = img.get(o..o + 16)?;
            let mut a = [0u8; 8];
            a.copy_from_slice(&s[..8]);
            let mut b = [0u8; 8];
            b.copy_from_slice(&s[8..]);
            Some((i64::from_le_bytes(a), u64::from_le_bytes(b)))
        };
        let mod0 = u32at(4)?;
        if img.get(mod0 as usize..mod0 as usize + 4)? != b"MOD0" {
            return None;
        }
        let dyn_ = mod0 + i64::from(u32at(mod0 + 4)? as u32 as i32);
        let mut tags: HashMap<i64, u64> = HashMap::new();
        for i in 0..4096 {
            let (t, v) = q(dyn_ + i * 16)?;
            if t == 0 {
                break;
            }
            tags.entry(t).or_insert(v);
        }
        let (symtab, strtab) = (*tags.get(&6)? as i64, *tags.get(&5)? as i64);
        let n = match tags.get(&4) {
            Some(h) => u32at(*h as i64 + 4)?,
            None => (strtab - symtab) / 24,
        };
        let mut out = HashMap::new();
        for i in 1..n {
            let b = usize::try_from(symtab + i * 24).ok()?;
            let e = img.get(b..b + 24)?;
            let name = i64::from(u32::from_le_bytes([e[0], e[1], e[2], e[3]]));
            let shndx = u16::from_le_bytes([e[6], e[7]]);
            let value = i64::from_le_bytes(e[8..16].try_into().ok()?);
            let size = i64::from_le_bytes(e[16..24].try_into().ok()?);
            if shndx != 0 {
                let s = usize::try_from(strtab + name).ok()?;
                let tail = img.get(s..)?;
                let z = tail.iter().position(|c| *c == 0)?;
                out.insert(
                    tail[..z].iter().map(|c| *c as char).collect::<String>(),
                    (value, size),
                );
            }
        }
        Some(out)
    }
    go(img).unwrap_or_default()
}
