//! PFS0 packages, common tickets and NCA headers: enough of LibHac's `PartitionFileSystem`, `Nca` and `NcaFsHeader` for
//! what `nx_extract` reads.

use std::sync::Arc;

use crate::aes::{Aes128, xts_decrypt};
use crate::error::{ExtractError, Result};
use crate::keys::{KEY_AREAS, KeySet};
use crate::storage::{Storage, SubStorage};

pub(crate) fn u32le(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4)
        .map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

pub(crate) fn u64le(b: &[u8], o: usize) -> u64 {
    b.get(o..o + 8).map_or(0, |s| {
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        u64::from_le_bytes(a)
    })
}

/// One file of a PFS0.
#[derive(Clone, Debug)]
pub struct PfsEntry {
    /// File name.
    pub name: String,
    /// Offset of the data from the PFS0's start.
    pub offset: u64,
    /// Size.
    pub size: u64,
}

/// Parses a PFS0 header from the start of `s`, offsets in the result are relative to the PFS0's start.
pub fn read_pfs0(s: &dyn Storage) -> Result<Vec<PfsEntry>> {
    read_partition_fs(s, b"PFS0", 24)
}

/// Parses an HFS0 header (a gamecard's partition: 0x40-byte entries, hash fields ignored) from the start of `s`.
pub fn read_hfs0(s: &dyn Storage) -> Result<Vec<PfsEntry>> {
    read_partition_fs(s, b"HFS0", 0x40)
}

fn read_partition_fs(s: &dyn Storage, magic: &[u8; 4], entry_size: u64) -> Result<Vec<PfsEntry>> {
    let kind = String::from_utf8_lossy(magic).into_owned();
    if s.size() < 16 {
        return Err(ExtractError::format(format!("truncated {kind} header")));
    }
    let h = s.read_vec(0, 16)?;
    if &h[0..4] != magic {
        return Err(ExtractError::format(format!("no {kind} magic")));
    }
    let count = u32le(&h, 4) as u64;
    let strings = u32le(&h, 8) as u64;
    let header_size = 16 + entry_size * count + strings;
    if header_size > s.size() || header_size > (64 << 20) {
        return Err(ExtractError::format(format!(
            "invalid {kind} file-name table size"
        )));
    }
    let hdr = s.read_vec(0, header_size as usize)?;
    let st = &hdr[(16 + entry_size * count) as usize..];
    let mut out = Vec::new();
    for i in 0..count as usize {
        let e = 16 + entry_size as usize * i;
        let name_off = u32le(&hdr, e + 16) as usize;
        if name_off as u64 >= strings {
            return Err(ExtractError::format(format!(
                "{kind} file-name offset outside the string table"
            )));
        }
        // names end at a NUL or (older NSZ writers) at the end of the table
        let rest = &st[name_off..];
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        let name = String::from_utf8_lossy(&rest[..end]).into_owned();
        let Some(offset) = header_size.checked_add(u64le(&hdr, e)) else {
            return Err(ExtractError::format(format!(
                "{kind} entry offset overflows"
            )));
        };
        let size = u64le(&hdr, e + 8);
        if offset.checked_add(size).is_none_or(|x| x > s.size()) {
            return Err(ExtractError::format(format!(
                "{kind} entry {name} outside the package"
            )));
        }
        out.push(PfsEntry { name, offset, size });
    }
    Ok(out)
}

/// The secure partition of a gamecard dump (`.xci`) as a storage: the card header's magic `HEAD` is at 0x100, the root
/// HFS0 lies where the header at 0x130 says, and its `secure` entry holds the NCAs (with their keys in the key area).
pub fn xci_secure(pkg: &Arc<dyn Storage>, file: &str) -> Result<Arc<dyn Storage>> {
    let root_at = u64le(&pkg.read_vec(0x130, 8)?, 0);
    if root_at < 0x200 || root_at >= pkg.size() {
        return Err(ExtractError::format(format!(
            "{file}: XCI root partition offset outside the file"
        )));
    }
    let root: Arc<dyn Storage> =
        Arc::new(SubStorage::new(pkg.clone(), root_at, pkg.size() - root_at)?);
    let parts = read_hfs0(root.as_ref())?;
    let secure = parts
        .iter()
        .find(|p| p.name == "secure")
        .ok_or_else(|| ExtractError::format(format!("no secure partition in {file}")))?;
    Ok(Arc::new(SubStorage::new(root, secure.offset, secure.size)?))
}

/// NCA content types (LibHac's `NcaContentType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentType {
    /// Program.
    Program,
    /// Meta.
    Meta,
    /// Control.
    Control,
    /// Manual (HtmlDocument, LegalInformation).
    Manual,
    /// Data.
    Data,
    /// PublicData.
    PublicData,
    /// Anything else.
    Other(u8),
}

/// A section's file-system header (the parts used here).
#[derive(Clone, Debug)]
pub struct FsHeader {
    /// The raw 0x200 bytes.
    pub raw: Vec<u8>,
    /// 0 RomFS, 1 PFS0.
    pub fs_type: u8,
    /// 2 hierarchical SHA-256, 3 IVFC.
    pub hash_type: u8,
    /// 1 none, 2 XTS, 3 CTR, 4 CTR-EX (BKTR).
    pub encryption: u8,
    /// Upper 8 counter bytes (big-endian of the header's little-endian value).
    pub upper: [u8; 8],
}

impl FsHeader {
    fn parse(raw: Vec<u8>) -> FsHeader {
        let fs_type = raw.get(2).copied().unwrap_or(0);
        let hash_type = raw.get(3).copied().unwrap_or(0);
        let encryption = raw.get(4).copied().unwrap_or(0);
        let upper = u64le(&raw, 0x140).to_be_bytes();
        FsHeader {
            raw,
            fs_type,
            hash_type,
            encryption,
            upper,
        }
    }

    /// A sparse layer (sparse info generation nonzero, LibHac's `ExistsSparseLayer`), which `nx_extract` reads only
    /// with an update (exit 4 without).
    pub fn is_sparse(&self) -> bool {
        self.raw.get(0x170..0x172).is_some_and(|g| g != [0, 0])
    }
}

/// A decrypted NCA header and where its sections lie.
pub struct Nca {
    /// Content ID (file name before the dot, lower case).
    pub id: String,
    /// The NCA's bytes inside the package.
    pub raw: Arc<dyn Storage>,
    /// Content type.
    pub content_type: ContentType,
    /// Program/title ID.
    pub title_id: u64,
    /// Key generation (max of the two header fields).
    pub key_generation: u8,
    /// Key area index.
    pub key_area: u8,
    /// Rights ID (zero without one).
    pub rights_id: [u8; 16],
    /// Per section: (start, end) in bytes from the NCA's start, and its header.
    pub sections: [Option<(u64, u64, FsHeader)>; 4],
    encrypted_key_area: [[u8; 16]; 4],
}

impl Nca {
    /// Opens an NCA: XTS-decrypts the 0xC00-byte header with `header_key`.
    pub fn open(ks: &KeySet, id: String, raw: Arc<dyn Storage>) -> Result<Nca> {
        let hk = ks
            .header_key()
            .ok_or_else(|| ExtractError::Keys("prod.keys has no header_key".into()))?;
        let mut h = raw.read_vec(0, 0xC00)?;
        let mut k1 = [0u8; 16];
        let mut k2 = [0u8; 16];
        k1.copy_from_slice(&hk[..16]);
        k2.copy_from_slice(&hk[16..]);
        let (a1, a2) = (Aes128::new(&k1), Aes128::new(&k2));
        k1 = [0; 16];
        k2 = [0; 16];
        let _ = (k1, k2);
        xts_decrypt(&a1, &a2, &mut h, 0x200, 0);
        if &h[0x200..0x204] != b"NCA3" {
            return Err(ExtractError::Unsupported(format!(
                "{id}: not an NCA3 header (wrong header_key, or NCA0/NCA2)"
            )));
        }
        let content_type = match h[0x205] {
            0 => ContentType::Program,
            1 => ContentType::Meta,
            2 => ContentType::Control,
            3 => ContentType::Manual,
            4 => ContentType::Data,
            5 => ContentType::PublicData,
            x => ContentType::Other(x),
        };
        let mut rights_id = [0u8; 16];
        rights_id.copy_from_slice(&h[0x230..0x240]);
        let mut sections: [Option<(u64, u64, FsHeader)>; 4] = Default::default();
        for (i, s) in sections.iter_mut().enumerate() {
            let start = u32le(&h, 0x240 + 16 * i) as u64 * 0x200;
            let end = u32le(&h, 0x244 + 16 * i) as u64 * 0x200;
            if end > start {
                let fs = FsHeader::parse(h[0x400 + 0x200 * i..0x600 + 0x200 * i].to_vec());
                *s = Some((start, end, fs));
            }
        }
        let mut encrypted_key_area = [[0u8; 16]; 4];
        for (i, k) in encrypted_key_area.iter_mut().enumerate() {
            k.copy_from_slice(&h[0x300 + 16 * i..0x310 + 16 * i]);
        }
        let key_generation = h[0x206].max(h[0x220]);
        let nca = Nca {
            id,
            raw,
            content_type,
            title_id: u64le(&h, 0x210),
            key_generation,
            key_area: h[0x207],
            rights_id,
            sections,
            encrypted_key_area,
        };
        h.iter_mut().for_each(|b| *b = 0);
        Ok(nca)
    }

    /// The index of the data (RomFS) section: 1 in a Program NCA (0 is the code), 0 in the others (LibHac's
    /// `NcaSectionType.Data`).
    pub fn data_section(&self) -> usize {
        usize::from(self.content_type == ContentType::Program)
    }

    /// Master key revision as LibHac derives it from the key generation.
    pub fn revision(&self) -> u8 {
        self.key_generation.saturating_sub(1)
    }

    /// Whether the NCA has a rights ID (title key from a ticket).
    pub fn has_rights_id(&self) -> bool {
        self.rights_id.iter().any(|&b| b != 0)
    }

    /// The AES-CTR section key: the title key (ticket) or key area entry 2, unwrapped.
    pub fn ctr_key(&self, ks: &KeySet) -> Result<Aes128> {
        let rev = self.revision();
        let mut k = if self.has_rights_id() {
            let kek = ks.titlekek(rev).ok_or_else(|| {
                ExtractError::Keys(format!("prod.keys has no titlekek_{rev:02x}"))
            })?;
            let mut t = ks.title_key(&self.rights_id).ok_or_else(|| {
                ExtractError::Keys(format!(
                    "{}: no title key for its rights ID: the package has no common ticket for it",
                    self.id
                ))
            })?;
            Aes128::new(&kek).decrypt_ecb(&mut t);
            t
        } else {
            let area = self.key_area;
            let kak = ks.key_area_key(area, rev).ok_or_else(|| {
                let name = KEY_AREAS.get(area as usize).copied().unwrap_or("?");
                ExtractError::Keys(format!("prod.keys has no key_area_key_{name}_{rev:02x}"))
            })?;
            let mut t = self.encrypted_key_area[2];
            Aes128::new(&kak).decrypt_ecb(&mut t);
            t
        };
        let aes = Aes128::new(&k);
        k = [0; 16];
        let _ = k;
        Ok(aes)
    }

    /// The raw (still encrypted) bytes of section `i`.
    pub fn section_raw(&self, i: usize) -> Result<(Arc<dyn Storage>, u64, FsHeader)> {
        let (start, end, fs) = self
            .sections
            .get(i)
            .and_then(|s| s.clone())
            .ok_or_else(|| ExtractError::format(format!("{}: no section {i}", self.id)))?;
        let sub: Arc<dyn Storage> =
            Arc::new(SubStorage::new(self.raw.clone(), start, end - start)?);
        Ok((sub, start, fs))
    }

    /// The raw (still encrypted) data of section `i` in the section's own offsets: the window of the NCA, or for a
    /// sparse section the view that maps its data ranges and its holes.
    pub fn section_view(&self, ks: &KeySet, i: usize) -> Result<(Arc<dyn Storage>, u64, FsHeader)> {
        let (start, fs) = match self.sections.get(i) {
            Some(Some((start, _, fs))) if fs.is_sparse() => (*start, fs.clone()),
            _ => return self.section_raw(i),
        };
        Ok((crate::sparse::open(ks, self, i)?, start, fs))
    }
}

/// Opens an NSP, or the secure partition of an XCI: imports the common tickets into `ks` and opens every NCA.
/// `file` names the package in messages.
pub fn open_nsp(ks: &mut KeySet, pkg: Arc<dyn Storage>) -> Result<Vec<Nca>> {
    open_package(ks, pkg, "the package")
}

/// [`open_nsp`] with the package's file name for the messages.
pub fn open_package(ks: &mut KeySet, pkg: Arc<dyn Storage>, file: &str) -> Result<Vec<Nca>> {
    let (pkg, entries) = if pkg.size() > 0x104 && pkg.read_vec(0x100, 4)? == b"HEAD" {
        let sec = xci_secure(&pkg, file)?;
        let e = read_hfs0(sec.as_ref())?;
        (sec, e)
    } else {
        let e = read_pfs0(pkg.as_ref())?;
        (pkg, e)
    };
    for e in &entries {
        if e.name.to_ascii_lowercase().ends_with(".tik") && e.size >= 0x2c0 {
            let t = pkg.read_vec(e.offset, 0x2c0)?;
            if t[0x281] != 0 {
                continue; // not a common ticket; nx_extract prints a [RED] line and goes on
            }
            let mut rid = [0u8; 16];
            let mut key = [0u8; 16];
            rid.copy_from_slice(&t[0x2a0..0x2b0]);
            key.copy_from_slice(&t[0x180..0x190]);
            ks.add_title_key(rid, key);
        }
    }
    let mut ncas = Vec::new();
    for e in &entries {
        let lower = e.name.to_ascii_lowercase();
        if lower.ends_with(".nca") {
            let id = lower.split('.').next().unwrap_or("").to_string();
            let raw: Arc<dyn Storage> = Arc::new(SubStorage::new(pkg.clone(), e.offset, e.size)?);
            ncas.push(Nca::open(ks, id, raw)?);
        } else if lower.ends_with(".ncz") {
            #[cfg(feature = "host")]
            {
                let id = lower.split('.').next().unwrap_or("").to_string();
                let raw: Arc<dyn Storage> =
                    Arc::new(SubStorage::new(pkg.clone(), e.offset, e.size)?);
                let ncz: Arc<dyn Storage> = Arc::new(crate::ncz::NczStorage::open(raw, &id)?);
                ncas.push(Nca::open(ks, id, ncz)?);
            }
            #[cfg(not(feature = "host"))]
            return Err(ExtractError::Unsupported(
                "NCZ extraction requires the host feature".into(),
            ));
        }
    }
    Ok(ncas)
}

/// LibHac's `Pick`: the NCA of a content type with the lowest title ID, or the one of `title`.
pub fn pick(ncas: &[Nca], t: ContentType, title: Option<u64>) -> Option<&Nca> {
    let mut r: Option<&Nca> = None;
    for n in ncas.iter().filter(|n| n.content_type == t) {
        let take = match title {
            None => r.is_none_or(|x| n.title_id < x.title_id),
            Some(id) => n.title_id == id,
        };
        if take {
            r = Some(n);
        }
    }
    r
}
