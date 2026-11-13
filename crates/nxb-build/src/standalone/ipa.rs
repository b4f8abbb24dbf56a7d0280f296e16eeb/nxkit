//! Deterministic unsigned Payload archive. ZIP64 permits large offsets and entry counts.
use nxb_pack::plist::{self, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, String>;

fn safe_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.chars().any(char::is_control)
        || path.contains(['\\', ':', '\0'])
        || path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
        || Path::new(path).is_absolute()
    {
        return Err(format!("unsafe bundle path {path:?}"));
    }
    Ok(())
}

pub(super) type Files = BTreeMap<String, Vec<u8>>;
pub(super) enum Source {
    Bytes(Vec<u8>),
    File(PathBuf),
}
impl Source {
    fn open(&self) -> Result<(Box<dyn Read + '_>, u64)> {
        match self {
            Self::Bytes(bytes) => Ok((Box::new(bytes.as_slice()), bytes.len() as u64)),
            Self::File(path) => {
                if !fs::symlink_metadata(path).map_err(err)?.is_file() {
                    return Err("bundle source is not a regular file".into());
                }
                let file = fs::File::open(path).map_err(err)?;
                let size = file.metadata().map_err(err)?.len();
                Ok((Box::new(file), size))
            }
        }
    }
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn word(b: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        b.get(at..at + 4)
            .ok_or("truncated host Mach-O")?
            .try_into()
            .map_err(|_| "bad host word")?,
    ))
}
fn validate_host(b: &[u8]) -> Result<()> {
    if word(b, 0)? != 0xfeedfacf || word(b, 4)? != 0x0100000c || word(b, 12)? != 2 {
        return Err("host must be an arm64 Mach-O executable".into());
    }
    let end = 32usize
        .checked_add(word(b, 20)? as usize)
        .ok_or("host command overflow")?;
    if end > b.len() {
        return Err("host commands truncated".into());
    }
    let mut at = 32;
    let mut ios = false;
    for _ in 0..word(b, 16)? {
        let cmd = word(b, at)?;
        let len = word(b, at + 4)? as usize;
        if len < 8 || !len.is_multiple_of(8) || at.checked_add(len).is_none_or(|n| n > end) {
            return Err("invalid host command".into());
        }
        if cmd == 0x32 {
            if len < 24 || word(b, at + 8)? != 2 {
                return Err("host is not built for iOS device".into());
            }
            ios = true;
        }
        if cmd == 0x1d && (len < 16 || word(b, at + 12)? != 0) {
            return Err("host contains a code signature, rebuild the unsigned host".into());
        }
        at += len;
    }
    if !ios || at != end {
        return Err("host has no valid iOS build version".into());
    }
    Ok(())
}
pub(super) fn read_host(root: &Path) -> Result<Files> {
    fn visit(root: &Path, path: &Path, files: &mut Files) -> Result<()> {
        for entry in fs::read_dir(path).map_err(err)? {
            let e = entry.map_err(err)?;
            let ty = e.file_type().map_err(err)?;
            if ty.is_symlink() {
                return Err("host app contains a symbolic link".into());
            }
            let p = e.path();
            let rel = p
                .strip_prefix(root)
                .map_err(err)?
                .to_str()
                .ok_or("host path not UTF-8")?
                .to_owned();
            safe_path(&rel)?;
            if rel
                .split('/')
                .any(|s| s == "_CodeSignature" || s == "embedded.mobileprovision")
            {
                return Err("host app is signed or provisioned".into());
            }
            if ty.is_dir() {
                visit(root, &p, files)?;
            } else if ty.is_file() {
                files.insert(rel, fs::read(p).map_err(err)?);
            } else {
                return Err("host app contains a special file".into());
            }
        }
        Ok(())
    }
    if fs::symlink_metadata(root)
        .map_err(err)?
        .file_type()
        .is_symlink()
    {
        return Err("host app must not be a symbolic link".into());
    }
    let mut files = Files::new();
    visit(root, root, &mut files)?;
    validate_host(files.get("nxkit").ok_or("host app lacks nxkit")?)?;
    let info =
        plist::parse(files.get("Info.plist").ok_or("host app lacks Info.plist")?).map_err(err)?;
    if info.get("CFBundleExecutable").and_then(Value::as_str) != Some("nxkit") {
        return Err("host CFBundleExecutable must be nxkit".into());
    }
    Ok(files)
}
fn reject_case_collisions<T>(files: &BTreeMap<String, T>) -> Result<()> {
    let mut spellings = BTreeMap::<String, String>::new();
    for path in files.keys() {
        let mut prefix = String::new();
        for component in path.trim_end_matches('/').split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            let folded = prefix.to_lowercase();
            if let Some(previous) = spellings.get(&folded) {
                if previous != &prefix {
                    return Err(format!(
                        "case-insensitive bundle path collision: {previous:?} and {prefix:?}"
                    ));
                }
            } else {
                spellings.insert(folded, prefix.clone());
            }
        }
    }
    Ok(())
}
pub(super) fn write(
    out: &Path,
    host: Files,
    files: Vec<(String, Vec<u8>)>,
    dirs: &[String],
    title: u64,
    name: &str,
) -> Result<()> {
    write_sources(
        out,
        host,
        files
            .into_iter()
            .map(|(p, b)| (p, Source::Bytes(b)))
            .collect(),
        dirs,
        title,
        name,
    )
}
pub(super) fn write_sources(
    out: &Path,
    mut host: Files,
    files: Vec<(String, Source)>,
    dirs: &[String],
    title: u64,
    name: &str,
) -> Result<()> {
    let mut info =
        plist::parse(host.get("Info.plist").ok_or("missing host plist")?).map_err(err)?;
    let Value::Dict(ref mut entries) = info else {
        return Err("host plist must be a dictionary".into());
    };
    for (key, value) in [
        ("CFBundleIdentifier", format!("app.nxkit.game.{title:016x}")),
        ("CFBundleDisplayName", name.into()),
        ("CFBundleName", name.into()),
    ] {
        entries.retain(|(k, _)| k != key);
        entries.push((key.into(), Value::String(value)));
    }
    host.insert(
        "Info.plist".into(),
        plist::to_cf_xml(&info).map_err(err)?.into_bytes(),
    );
    let mut host: BTreeMap<_, _> = host
        .into_iter()
        .map(|(p, b)| (p, Source::Bytes(b)))
        .collect();
    for (path, bytes) in files {
        safe_path(&path)?;
        if host.insert(path.clone(), bytes).is_some() {
            return Err(format!("duplicate bundle entry {path}"));
        }
    }
    for dir in dirs.iter().map(String::as_str).chain(["romfs"]) {
        safe_path(dir)?;
        if host.contains_key(dir) {
            return Err("file conflicts with directory".into());
        }
        host.entry(format!("{dir}/"))
            .or_insert_with(|| Source::Bytes(Vec::new()));
    }
    reject_case_collisions(&host)?;
    // Validate file/parent collisions before opening any output.
    for path in host.keys() {
        let mut p = Path::new(path);
        while let Some(parent) = p.parent() {
            if parent.as_os_str().is_empty() {
                break;
            }
            if host.contains_key(parent.to_str().ok_or("bad parent")?) {
                return Err("bundle file used as a directory".into());
            }
            p = parent;
        }
    }
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(err)?;
    }
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)
        .map_err(err)?;
    let result = archive(&mut f, &host).and_then(|()| f.sync_all().map_err(err));
    if result.is_err() {
        drop(f);
        let _ = fs::remove_file(out);
    }
    result
}
fn crc32(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320u32 & 0u32.wrapping_sub(crc & 1));
        }
    }
    crc
}
fn u16b(b: &mut Vec<u8>, n: u16) {
    b.extend(n.to_le_bytes());
}
fn u32b(b: &mut Vec<u8>, n: u32) {
    b.extend(n.to_le_bytes());
}
fn u64b(b: &mut Vec<u8>, n: u64) {
    b.extend(n.to_le_bytes());
}
fn archive(out: &mut (impl Write + Seek), files: &BTreeMap<String, Source>) -> Result<()> {
    let mut central = Vec::new();
    for (path, data) in files {
        let name = format!("Payload/Nxkit.app/{path}");
        let len = u16::try_from(name.len()).map_err(|_| "ZIP name too long")?;
        let offset = out.stream_position().map_err(err)?;
        let (mut source, size) = data.open()?;
        let mut h = Vec::new();
        u32b(&mut h, 0x04034b50);
        for n in [45, 0x800, 0, 0, 33] {
            u16b(&mut h, n);
        }
        u32b(&mut h, 0);
        u32b(&mut h, u32::MAX);
        u32b(&mut h, u32::MAX);
        u16b(&mut h, len);
        u16b(&mut h, 20);
        h.extend(name.as_bytes());
        u16b(&mut h, 1);
        u16b(&mut h, 16);
        u64b(&mut h, size);
        u64b(&mut h, size);
        out.write_all(&h).map_err(err)?;
        let mut crc = !0u32;
        let mut copied = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let n = source.read(&mut buffer).map_err(err)?;
            if n == 0 {
                break;
            }
            copied = copied
                .checked_add(n as u64)
                .ok_or("ZIP source size overflow")?;
            if copied > size {
                return Err("bundle source changed while packaging".into());
            }
            crc = crc32(crc, &buffer[..n]);
            out.write_all(&buffer[..n]).map_err(err)?;
        }
        if copied != size {
            return Err("bundle source changed while packaging".into());
        }
        let crc = !crc;
        let end = out.stream_position().map_err(err)?;
        out.seek(SeekFrom::Start(offset + 14)).map_err(err)?;
        out.write_all(&crc.to_le_bytes()).map_err(err)?;
        out.seek(SeekFrom::Start(end)).map_err(err)?;
        u32b(&mut central, 0x02014b50);
        for n in [0x32d, 45, 0x800, 0, 0, 33] {
            u16b(&mut central, n);
        }
        u32b(&mut central, crc);
        u32b(&mut central, u32::MAX);
        u32b(&mut central, u32::MAX);
        for n in [len, 28, 0, 0, 0] {
            u16b(&mut central, n);
        }
        let mode = if path.ends_with('/') {
            0o40755u32
        } else if path == "nxkit" || path.ends_with(".dylib") {
            0o100755
        } else {
            0o100644
        };
        u32b(
            &mut central,
            (mode << 16) | if path.ends_with('/') { 16 } else { 0 },
        );
        u32b(&mut central, u32::MAX);
        central.extend(name.as_bytes());
        u16b(&mut central, 1);
        u16b(&mut central, 24);
        u64b(&mut central, size);
        u64b(&mut central, size);
        u64b(&mut central, offset);
    }
    let start = out.stream_position().map_err(err)?;
    out.write_all(&central).map_err(err)?;
    let end = out.stream_position().map_err(err)?;
    let mut tail = Vec::new();
    u32b(&mut tail, 0x06064b50);
    u64b(&mut tail, 44);
    u16b(&mut tail, 0x32d);
    u16b(&mut tail, 45);
    u32b(&mut tail, 0);
    u32b(&mut tail, 0);
    u64b(&mut tail, files.len() as u64);
    u64b(&mut tail, files.len() as u64);
    u64b(&mut tail, central.len() as u64);
    u64b(&mut tail, start);
    u32b(&mut tail, 0x07064b50);
    u32b(&mut tail, 0);
    u64b(&mut tail, end);
    u32b(&mut tail, 1);
    u32b(&mut tail, 0x06054b50);
    u32b(&mut tail, 0);
    u16b(&mut tail, u16::MAX);
    u16b(&mut tail, u16::MAX);
    u32b(&mut tail, u32::MAX);
    u32b(&mut tail, u32::MAX);
    u16b(&mut tail, 0);
    out.write_all(&tail).map_err(err)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ambiguous_paths() {
        for path in [
            "../game", "a/../b", "/game", "a\\b", "a//b", "a\nb", "a\rb", "a\tb", "a\u{7f}b",
        ] {
            assert!(safe_path(path).is_err());
        }
    }
    #[test]
    fn rejects_case_collisions_in_files_and_parent_directories() {
        for paths in [
            ["game.json", "GAME.JSON"],
            ["romfs/a", "RomFS/b"],
            ["Ä/a", "ä/b"],
        ] {
            let files = paths
                .into_iter()
                .map(|p| (p.to_string(), Vec::<u8>::new()))
                .collect();
            assert!(reject_case_collisions(&files).is_err());
        }
        let files = ["romfs/", "romfs/a", "romfs/b"]
            .into_iter()
            .map(|p| (p.to_string(), Vec::<u8>::new()))
            .collect();
        assert!(reject_case_collisions(&files).is_ok());
    }
    #[test]
    fn rejects_file_directory_collision_before_opening_output() {
        let out =
            std::env::temp_dir().join(format!("nxb-ipa-collision-{}.ipa", std::process::id()));
        let host = Files::from([("Info.plist".into(), b"<plist><dict/></plist>".to_vec())]);
        let error = write(
            &out,
            host,
            vec![("romfs/data".into(), vec![])],
            &["romfs/data".into()],
            0,
            "test",
        )
        .unwrap_err();
        assert_eq!(error, "file conflicts with directory");
        assert!(!out.exists());
    }
    #[test]
    fn deterministic_zip64_and_crc() {
        assert_eq!(!crc32(!0, b"123456789"), 0xcbf43926);
        let files = BTreeMap::from([
            ("romfs/".into(), Source::Bytes(vec![])),
            ("game.json".into(), Source::Bytes(b"{}".to_vec())),
        ]);
        let mut a = std::io::Cursor::new(vec![]);
        let mut b = std::io::Cursor::new(vec![]);
        archive(&mut a, &files).unwrap();
        archive(&mut b, &files).unwrap();
        assert_eq!(a, b);
        assert!(a.get_ref().windows(4).any(|s| s == b"PK\x06\x06"));
    }
    #[test]
    fn streamed_files_match_memory_entries_and_known_crc() {
        let root = std::env::temp_dir().join(format!("nxb-ipa-stream-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("romfs.bin");
        let bytes: Vec<u8> = (0..150_001).map(|i| (i % 251) as u8).collect();
        fs::write(&path, &bytes).unwrap();
        let memory = BTreeMap::from([("romfs/data".into(), Source::Bytes(bytes))]);
        let disk = BTreeMap::from([("romfs/data".into(), Source::File(path.clone()))]);
        let mut a = std::io::Cursor::new(vec![]);
        let mut b = std::io::Cursor::new(vec![]);
        archive(&mut a, &memory).unwrap();
        archive(&mut b, &disk).unwrap();
        assert_eq!(a, b);
        // Independently calculated using Python's zlib.crc32.
        assert_eq!(
            u32::from_le_bytes(b.get_ref()[14..18].try_into().unwrap()),
            0xe03568e0
        );
        fs::write(&path, []).unwrap();
        archive(&mut b, &disk).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(archive(&mut b, &disk).is_err());
        fs::remove_dir(&root).unwrap();
    }
    #[test]
    fn failed_stream_removes_partial_output() {
        let root = std::env::temp_dir().join(format!("nxb-ipa-fail-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let out = root.join("test.ipa");
        let host = Files::from([(
            "Info.plist".into(),
            b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict/></plist>".to_vec(),
        )]);
        assert!(
            write_sources(
                &out,
                host,
                vec![("romfs/data".into(), Source::File(root.join("missing")))],
                &[],
                0,
                "test"
            )
            .is_err()
        );
        assert!(!out.exists());
        fs::remove_dir(&root).unwrap();
    }
    #[test]
    fn rejects_macos_and_signed_host() {
        let mut b = vec![0u8; 72];
        for (at, n) in [
            (0, 0xfeedfacfu32),
            (4, 0x0100000c),
            (12, 2),
            (16, 1),
            (20, 24),
            (32, 0x32),
            (36, 24),
            (40, 1),
        ] {
            b[at..at + 4].copy_from_slice(&n.to_le_bytes());
        }
        assert!(validate_host(&b).is_err());
        b[40..44].copy_from_slice(&2u32.to_le_bytes());
        assert!(validate_host(&b).is_ok());
        b[16..20].copy_from_slice(&2u32.to_le_bytes());
        b[20..24].copy_from_slice(&40u32.to_le_bytes());
        b[56..60].copy_from_slice(&0x1du32.to_le_bytes());
        b[60..64].copy_from_slice(&16u32.to_le_bytes());
        b[68..72].copy_from_slice(&8u32.to_le_bytes());
        assert!(validate_host(&b).is_err());
    }
}
