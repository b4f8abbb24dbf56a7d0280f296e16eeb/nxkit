//! The imports and exports of a Switch module (NSO), read from the dynamic section of its flat image.
//!
//! An NSO is a shared object linked at address 0. Its text starts with a branch and the offset of the `MOD0` header at
//! +4. `MOD0` holds the offset of the dynamic section (an i32 from `MOD0`). The slots are the `r_offset` of the
//! relocations (`DT_JMPREL` and `DT_RELA`) that name an undefined symbol. Weak undefined symbols are imports too, with
//! the weak flag set. The exports are the defined global symbols of default visibility.
//! This is the ELF64 AArch64 layout: 16-byte dynamic entries, 24-byte symbols and 24-byte `Rela` entries. Not yet
//! checked against a real NSO.

/// One name with its slot (imports) or its value (exports), as an offset from the module's base.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sym {
    /// The symbol's name.
    pub name: String,
    /// The offset from the base: the slot the loader fills (import) or the symbol's address (export).
    pub slot: u64,
}

/// One slot that the loader fills with the address of an undefined symbol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Import {
    /// The symbol's name.
    pub name: String,
    /// The offset from the base of the slot.
    pub slot: u64,
    /// The addend of the relocation. A `JUMP_SLOT` has 0, because the loader stores the address alone.
    pub addend: i64,
    /// Whether the symbol is weak. A weak symbol that nothing provides is stored as 0.
    pub weak: bool,
}

/// The dynamic symbols of one module.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Symbols {
    /// Symbols the module needs, one entry for each slot, in slot order.
    pub imports: Vec<Import>,
    /// Symbols the module defines.
    pub exports: Vec<Sym>,
}

const DT_NULL: u64 = 0;
const DT_PLTRELSZ: u64 = 2;
const DT_HASH: u64 = 4;
const DT_STRTAB: u64 = 5;
const DT_SYMTAB: u64 = 6;
const DT_RELA: u64 = 7;
const DT_RELASZ: u64 = 8;
const DT_JMPREL: u64 = 23;
const R_NONE: u64 = 0;
const R_JUMP_SLOT: u64 = 1026;
const STB_WEAK: u8 = 2;
const DT_GNU_HASH: u64 = 0x6fff_fef5;
const MAX_SYMBOLS: usize = 1 << 20;

fn u32at(d: &[u8], o: usize) -> Result<u32, String> {
    d.get(o..o + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| format!("dynamic section: read at {o:#x} is outside the image"))
}

fn u64at(d: &[u8], o: usize) -> Result<u64, String> {
    d.get(o..o + 8)
        .map(|s| u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
        .ok_or_else(|| format!("dynamic section: read at {o:#x} is outside the image"))
}

fn cstr(d: &[u8], o: usize) -> Result<String, String> {
    let tail = d
        .get(o..)
        .ok_or_else(|| format!("string table: offset {o:#x} is outside the image"))?;
    let n = tail.iter().position(|b| *b == 0).unwrap_or(tail.len());
    Ok(String::from_utf8_lossy(&tail[..n]).into_owned())
}

/// The number of dynamic symbols: `nchain` of `DT_HASH`, else the last chain of `DT_GNU_HASH`, else the distance
/// from the symbol table to the string table.
fn symbol_count(
    d: &[u8],
    hash: Option<usize>,
    gnu: Option<usize>,
    symtab: usize,
    strtab: usize,
) -> Result<usize, String> {
    if let Some(h) = hash {
        return Ok(u32at(d, h + 4)? as usize);
    }
    if let Some(g) = gnu {
        let nbuckets = u32at(d, g)? as usize;
        let symoffset = u32at(d, g + 4)? as usize;
        let bloom = u32at(d, g + 8)? as usize;
        let buckets = g + 16 + bloom * 8;
        let chains = buckets + nbuckets * 4;
        let mut last = 0usize;
        for i in 0..nbuckets {
            last = last.max(u32at(d, buckets + i * 4)? as usize);
        }
        if last < symoffset {
            return Ok(symoffset);
        }
        let mut i = last;
        loop {
            let h = u32at(d, chains + (i - symoffset) * 4)?;
            i += 1;
            if h & 1 != 0 || i > MAX_SYMBOLS {
                break;
            }
        }
        return Ok(i);
    }
    strtab.checked_sub(symtab).map(|n| n / 24).ok_or_else(|| {
        "dynamic section: no hash table and the string table is before the symbol table".to_string()
    })
}

/// Reads the imports and exports of the module whose flat image is `image`.
pub(crate) fn symbols(image: &[u8]) -> Result<Symbols, String> {
    let mod0 = u32at(image, 4)? as usize;
    if image.get(mod0..mod0 + 4) != Some(b"MOD0") {
        return Err("the module has no MOD0 header".into());
    }
    let dynamic = (mod0 as i64 + i64::from(u32at(image, mod0 + 4)? as i32)) as usize;
    let mut tags = std::collections::BTreeMap::new();
    for i in 0..4096 {
        let tag = u64at(image, dynamic + i * 16)?;
        if tag == DT_NULL {
            break;
        }
        tags.entry(tag)
            .or_insert(u64at(image, dynamic + i * 16 + 8)? as usize);
    }
    let need = |t: u64, what: &str| {
        tags.get(&t)
            .copied()
            .ok_or_else(|| format!("dynamic section: no {what}"))
    };
    let (symtab, strtab) = (need(DT_SYMTAB, "DT_SYMTAB")?, need(DT_STRTAB, "DT_STRTAB")?);
    let count = symbol_count(
        image,
        tags.get(&DT_HASH).copied(),
        tags.get(&DT_GNU_HASH).copied(),
        symtab,
        strtab,
    )?
    .min(MAX_SYMBOLS);
    // (name, defined, value, binding, hidden) per symbol
    let mut syms = Vec::with_capacity(count);
    for i in 0..count {
        let o = symtab + i * 24;
        let name = u32at(image, o)? as usize;
        let info = *image.get(o + 4).ok_or("symbol table: outside the image")?;
        let shndx = image
            .get(o + 6..o + 8)
            .map(|s| u16::from_le_bytes([s[0], s[1]]))
            .ok_or("symbol table: outside the image")?;
        let other = *image.get(o + 5).ok_or("symbol table: outside the image")?;
        let value = u64at(image, o + 8)?;
        syms.push((
            cstr(image, strtab + name)?,
            shndx != 0,
            value,
            info >> 4,
            other & 3 != 0,
        ));
    }
    let mut out = Symbols::default();
    for (name, defined, value, bind, hidden) in syms.iter().skip(1) {
        if *defined && matches!(*bind, 1 | 2) && !*hidden && *value != 0 && !name.is_empty() {
            out.exports.push(Sym {
                name: name.clone(),
                slot: *value,
            });
        }
    }
    for (rel, size) in [(DT_JMPREL, DT_PLTRELSZ), (DT_RELA, DT_RELASZ)] {
        let (Some(start), Some(len)) = (tags.get(&rel).copied(), tags.get(&size).copied()) else {
            continue;
        };
        for i in 0..len / 24 {
            let o = start + i * 24;
            let (slot, info, addend) = (
                u64at(image, o)?,
                u64at(image, o + 8)?,
                u64at(image, o + 16)?,
            );
            let (sym, kind) = ((info >> 32) as usize, info & 0xffff_ffff);
            if kind == R_NONE {
                continue;
            }
            if let Some((name, false, _, bind, _)) = syms.get(sym).filter(|_| sym != 0)
                && !name.is_empty()
            {
                out.imports.push(Import {
                    name: name.clone(),
                    slot,
                    addend: if kind == R_JUMP_SLOT {
                        0
                    } else {
                        addend as i64
                    },
                    weak: *bind == STB_WEAK,
                });
            }
        }
    }
    out.imports.sort_by_key(|s| s.slot);
    out.exports.sort_by_key(|s| s.slot);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put32(d: &mut [u8], o: usize, v: u32) {
        d[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn put64(d: &mut [u8], o: usize, v: u64) {
        d[o..o + 8].copy_from_slice(&v.to_le_bytes());
    }

    /// A module with one import (`nnFoo` at slot 0x300) and one export (`nnBar` at 0x40).
    fn module() -> Vec<u8> {
        let mut d = vec![0u8; 0x400];
        put32(&mut d, 4, 0x10);
        d[0x10..0x14].copy_from_slice(b"MOD0");
        put32(&mut d, 0x14, 0x30); // dynamic at 0x40
        let dynamic = 0x40;
        let tags = [
            (DT_HASH, 0x100u64),
            (DT_STRTAB, 0x180),
            (DT_SYMTAB, 0x120),
            (DT_JMPREL, 0x1c0),
            (DT_PLTRELSZ, 24),
        ];
        for (i, (t, v)) in tags.iter().enumerate() {
            put64(&mut d, dynamic + i * 16, *t);
            put64(&mut d, dynamic + i * 16 + 8, *v);
        }
        put32(&mut d, 0x104, 3); // nchain
        // symbols: 0 null, 1 nnFoo undefined, 2 nnBar defined
        d[0x180..0x190].copy_from_slice(b"\0nnFoo\0nnBar\0\0\0\0");
        put32(&mut d, 0x120 + 24, 1);
        d[0x120 + 24 + 4] = 0x12;
        put32(&mut d, 0x120 + 48, 7);
        d[0x120 + 48 + 4] = 0x12;
        d[0x120 + 48 + 6] = 1;
        put64(&mut d, 0x120 + 48 + 8, 0x40);
        put64(&mut d, 0x1c0, 0x300);
        put64(&mut d, 0x1c8, (1u64 << 32) | 1026);
        d
    }

    #[test]
    fn reads_imports_and_exports() {
        let s = symbols(&module()).unwrap();
        assert_eq!(
            s.imports,
            [Import {
                name: "nnFoo".into(),
                slot: 0x300,
                addend: 0,
                weak: false
            }]
        );
        assert_eq!(
            s.exports,
            [Sym {
                name: "nnBar".into(),
                slot: 0x40
            }]
        );
    }

    #[test]
    fn a_weak_import_keeps_its_addend_and_a_hidden_symbol_is_not_exported() {
        let mut d = module();
        d[0x120 + 24 + 4] = 0x22; // nnFoo is weak
        d[0x120 + 48 + 5] = 2; // nnBar is hidden
        put32(&mut d, 0x1c8, 1025); // GLOB_DAT
        put64(&mut d, 0x1d0, 0x18);
        let s = symbols(&d).unwrap();
        assert_eq!(
            s.imports,
            [Import {
                name: "nnFoo".into(),
                slot: 0x300,
                addend: 0x18,
                weak: true
            }]
        );
        assert!(s.exports.is_empty());
    }

    #[test]
    fn refuses_a_module_without_mod0() {
        let mut d = module();
        d[0x10] = b'X';
        assert!(symbols(&d).is_err());
    }
}
