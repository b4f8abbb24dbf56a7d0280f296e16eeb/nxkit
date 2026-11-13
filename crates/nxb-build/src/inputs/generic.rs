//! Metadata-driven input selection for generic builds.

use super::*;

/// How a package was supplied. Explicit additions must belong to the selected game.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GenericRole {
    Input,
    Update,
    Dlc,
}

type GenericPackage = (PathBuf, GenericRole, Metas);

fn generic_family(kind: &str, title: u64) -> Result<u64, String> {
    let low = title & 0xFFF;
    match kind {
        "Application" if low == 0 => Ok(title),
        "Patch" if low == 0x800 => Ok(title - 0x800),
        "AddOnContent" if (1..=2000).contains(&low) => (title & !0xFFF)
            .checked_sub(0x1000)
            .ok_or_else(|| format!("invalid DLC title ID {title:016X}")),
        _ => Err(format!("invalid {kind} title ID {title:016X}")),
    }
}

/// Enforce the membership assumptions used by the shared extraction entry points.
fn checked_metadata(
    metas: Vec<nxb_extract::meta::Cnmt>,
    ncas: &[(&str, nxb_extract::nca::ContentType, u64)],
) -> Result<Metas, String> {
    use nxb_extract::nca::ContentType;
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for meta in metas {
        let kind = match meta.kind {
            0x80 => "Application",
            0x81 => "Patch",
            0x82 => "AddOnContent",
            _ => continue,
        };
        if !seen.insert((meta.kind, meta.title)) {
            return Err(format!(
                "multiple {kind} content metas for {:016X} in one package are unsupported",
                meta.title
            ));
        }
        let app = generic_family(kind, meta.title)?;
        let listed = |id: &str| meta.contents.iter().any(|content| content == id);
        if meta.kind == 0x82 {
            // aoc() selects by title only. Reject competing candidates even if unlisted.
            let data: Vec<_> = ncas
                .iter()
                .filter(|(_, ty, title)| {
                    *title == meta.title
                        && matches!(ty, ContentType::PublicData | ContentType::Data)
                })
                .collect();
            if data.len() != 1 || !listed(data[0].0) {
                return Err(format!(
                    "DLC {:016X} needs exactly one same-title Data/PublicData NCA listed by its content meta",
                    meta.title
                ));
            }
        } else {
            let programs: Vec<_> = ncas
                .iter()
                .filter(|(id, ty, _)| *ty == ContentType::Program && listed(id))
                .collect();
            if programs.len() != 1 || programs[0].2 != app {
                return Err(format!(
                    "{kind} {:016X} needs exactly one listed Program NCA of application {app:016X}; missing, renamed or multi-program content is unsupported",
                    meta.title
                ));
            }
        }
        out.push((kind.to_string(), meta.title, meta.version));
    }
    Ok(out)
}

fn generic_contents(key_text: &str, path: &Path) -> Result<Metas, String> {
    use nxb_extract::storage::{FileStorage, Storage};
    use std::sync::Arc;
    let read = || -> nxb_extract::error::Result<_> {
        let mut keys = nxb_extract::keys::KeySet::parse(key_text);
        let storage: Arc<dyn Storage> = Arc::new(FileStorage::open(path)?);
        let ncas = nxb_extract::nca::open_package(&mut keys, storage, &path.display().to_string())?;
        let cache = Arc::new(nxb_extract::cache::BlockCache::new(16));
        let metas = nxb_extract::meta::metas(&keys, &ncas, &cache)?;
        Ok((metas, ncas))
    };
    let (metas, ncas) =
        read().map_err(|e| crate::exit::fail(e.exit_code(), format!("{}: {e}", path.display())))?;
    let headers: Vec<_> = ncas
        .iter()
        .map(|n| (n.id.as_str(), n.content_type, n.title_id))
        .collect();
    checked_metadata(metas, &headers).map_err(|e| format!("{}: {e}", path.display()))
}

/// Select from actual content metadata, never from names or ticket hints.
fn select_generic(packages: Vec<GenericPackage>, want: Option<u64>) -> Result<Inputs, String> {
    let mut families: BTreeMap<u64, Family> = BTreeMap::new();
    let mut base_versions = BTreeMap::new();
    let mut primary = std::collections::BTreeSet::new();
    let mut explicit = Vec::new();
    // Sorting makes equal-version copies independent of argument order.
    let mut packages = packages;
    packages.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, role, metas) in packages {
        if metas.is_empty() {
            return Err(format!(
                "{} has no readable application content metadata",
                path.display()
            ));
        }
        let required = match role {
            GenericRole::Input => None,
            GenericRole::Update => Some("Patch"),
            GenericRole::Dlc => Some("AddOnContent"),
        };
        if let Some(kind) = required
            && !metas.iter().any(|m| m.0 == kind)
        {
            return Err(format!("{} has no {kind} content metadata", path.display()));
        }
        for (kind, title, version) in metas {
            let app = generic_family(&kind, title)?;
            if role == GenericRole::Input {
                primary.insert(app);
            } else {
                explicit.push((path.clone(), app));
            }
            let family = families.entry(app).or_default();
            match kind.as_str() {
                "Application" => {
                    if family.base.is_none() && role == GenericRole::Input {
                        family.base = Some(path.clone());
                        base_versions.insert(app, version);
                    }
                }
                "Patch" => family.updates.push((version, path.clone())),
                "AddOnContent" => family.dlc.push((title, version, path.clone())),
                _ => {}
            }
        }
    }
    let app = match want {
        Some(app) if primary.contains(&app) => app,
        Some(app) => return Err(format!("no game {app:016X} (--title-id) in the input")),
        None if primary.len() == 1 => primary.iter().next().copied().unwrap_or(0),
        None if primary.is_empty() => return Err("no game package found in the input".into()),
        None => return Err("more than one game in the input; select one with --title-id".into()),
    };
    for (path, other) in explicit {
        if other != app {
            return Err(format!(
                "{} belongs to {other:016X}, not selected game {app:016X}",
                path.display()
            ));
        }
    }
    let family = families.remove(&app).unwrap_or_default();
    let base = family
        .base
        .ok_or_else(|| format!("no base game package found for {app:016X}"))?;
    let mut latest = BTreeMap::new();
    for (title, version, path) in family.dlc {
        let entry = latest.entry(title).or_insert((version, path.clone()));
        if version > entry.0 {
            *entry = (version, path);
        }
    }
    let mut updates = family.updates;
    // Higher versions first, then lexicographic path for equal versions.
    updates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    updates.truncate(1);
    Ok(Inputs {
        source: Source::Packages,
        app,
        base,
        base_version: base_versions.get(&app).copied().unwrap_or(0),
        updates,
        dlc: latest.into_iter().map(|(t, (v, p))| (t, v, p)).collect(),
        app_flag: Some(app),
        has_base: true,
        notes: Vec::new(),
    })
}

fn generic_package_paths(path: &Path) -> Result<Vec<PathBuf>, String> {
    if path.is_dir() {
        let packages = files_with(path, &SUFFIXES);
        if packages.is_empty() {
            return Err(format!(
                "no packages (.nsp, .nsz, .xci, .xcz) in {}",
                path.display()
            ));
        }
        Ok(packages)
    } else if path.is_file() && SUFFIXES.contains(&suffix(path).as_str()) {
        Ok(vec![path.to_path_buf()])
    } else {
        Err(format!("not a package file or folder: {}", path.display()))
    }
}

fn generic_keys(a: &Args) -> Result<String, String> {
    let keys = crate::extract_stage::keys(a)?;
    let key_text = std::fs::read_to_string(&keys).map_err(|e| {
        crate::exit::fail(
            6,
            format!("cannot read keys file {} as UTF-8: {e}", keys.display()),
        )
    })?;
    if nxb_extract::keys::KeySet::parse(&key_text)
        .header_key()
        .is_none()
    {
        return Err(crate::exit::fail(
            6,
            "the keys file has no valid header_key",
        ));
    }
    Ok(key_text)
}

fn merge_nup_dlc(
    app: u64,
    mut dlc: Vec<(u64, u32, PathBuf)>,
    packages: Vec<GenericPackage>,
) -> Result<Vec<(u64, u32, PathBuf)>, String> {
    for (path, _, metas) in packages {
        if !metas.iter().any(|m| m.0 == "AddOnContent") {
            return Err(format!(
                "{} has no AddOnContent content metadata",
                path.display()
            ));
        }
        for (kind, title, version) in metas {
            if generic_family(&kind, title)? != app {
                return Err(format!(
                    "{} does not belong to selected game {app:016X}",
                    path.display()
                ));
            }
            if kind == "AddOnContent" {
                dlc.push((title, version, path.clone()));
            }
        }
    }
    // Highest version first, then path, independent of argument order.
    dlc.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    dlc.dedup_by_key(|d| d.0);
    Ok(dlc)
}

fn mixed_package_paths(path: &Path) -> Result<Vec<PathBuf>, String> {
    if path.is_dir() {
        let files = files_with(path, &["nsp", "nsz", "xci", "xcz", "nup"]);
        if files.is_empty() {
            return Err(format!("no packages in {}", path.display()));
        }
        Ok(files)
    } else if path.is_file() && nxb_extract::nup::is_nup(path) {
        Ok(vec![path.to_path_buf()])
    } else {
        generic_package_paths(path)
    }
}

fn external_dlc(a: &Args, mut key_text: Option<String>) -> Result<Vec<GenericPackage>, String> {
    let mut packages = Vec::new();
    for input in &a.aoc {
        let paths = mixed_package_paths(input)?;
        for path in paths {
            let metas = if nxb_extract::nup::is_nup(&path) {
                let nup = nxb_extract::nup::Nup::open(&path)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                let records = nxb_extract::nup::addon_records(&nup)
                    .map_err(|e| crate::exit::fail(e.exit_code(), e.to_string()))?;
                if records.is_empty() {
                    return Err(format!("{} has no add-on content records", path.display()));
                }
                records
                    .into_keys()
                    .map(|(title, version)| ("AddOnContent".into(), title, version))
                    .collect()
            } else {
                if key_text.is_none() {
                    key_text = Some(generic_keys(a)?);
                }
                generic_contents(key_text.as_deref().unwrap_or_default(), &path)?
            };
            packages.push((path, GenericRole::Dlc, metas));
        }
    }
    Ok(packages)
}

fn merge_package_updates(
    mut input: Inputs,
    mut packages: Vec<GenericPackage>,
) -> Result<Inputs, String> {
    packages.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, _, metas) in packages {
        if !metas.iter().any(|m| m.0 == "Patch") {
            return Err(format!("{} has no Patch content metadata", path.display()));
        }
        for (kind, title, version) in metas {
            if generic_family(&kind, title)? != input.app {
                return Err(format!(
                    "{} does not belong to selected game {:016X}",
                    path.display(),
                    input.app
                ));
            }
            if kind == "Patch" {
                let current = input.updates.last().map_or(input.base_version, |u| u.0);
                if version > current {
                    input.updates = vec![(version, path.clone())];
                }
            } else if kind == "AddOnContent" {
                input.dlc.push((title, version, path.clone()));
            }
        }
    }
    input.dlc = merge_nup_dlc(input.app, input.dlc, Vec::new())?;
    Ok(input)
}

fn generic_nup(
    path: &Path,
    a: &Args,
    want: Option<u64>,
    standalone_update: bool,
) -> Result<Inputs, String> {
    let n = nxb_extract::nup::Nup::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let s =
        nxb_extract::nup::scan(&n).map_err(|e| crate::exit::fail(e.exit_code(), e.to_string()))?;
    // NUP records use the application ID in reference dumps. Also accept the
    // patch ID used by other producers. The shared scan verifies NPDM identity.
    if s.base.is_some_and(|i| n.packages[i].title_id != s.app)
        || s.updates.values().any(|i| {
            let title = n.packages[*i].title_id;
            title != s.app && title != (s.app | 0x800)
        })
    {
        return Err(format!(
            "NUP base or update title ID does not match application {:016X}",
            s.app
        ));
    }
    if s.base.is_none()
        && (s.updates.is_empty()
            || s.updates.values().any(|i| {
                let package = &n.packages[*i];
                package.flags & 1 == 0 || package.exefs == u32::MAX || package.romfs == u32::MAX
            }))
    {
        return Err(
            "no base game: a standalone NUP update needs complete exefs/ and romfs/".into(),
        );
    }
    if standalone_update {
        let latest = s.updates.values().next_back().map(|i| &n.packages[*i]);
        if latest.is_none_or(|p| p.flags & 1 == 0 || p.exefs == u32::MAX || p.romfs == u32::MAX) {
            return Err(format!(
                "{}: external NUP update must be marked standalone with complete ExeFS and RomFS",
                path.display()
            ));
        }
    }
    if let Some(w) = want
        && w != s.app
    {
        return Err(format!(
            "no game {w:016X} (--title-id) in {}",
            path.display()
        ));
    }
    let embedded_dlc = s
        .dlc
        .iter()
        .map(|(t, i)| (*t, n.packages[*i].version, path.to_path_buf()))
        .collect();
    let dlc = merge_nup_dlc(s.app, embedded_dlc, external_dlc(a, None)?)?;
    let input = Inputs {
        source: Source::Nup,
        app: s.app,
        base: path.to_path_buf(),
        base_version: s.base.map_or(0, |i| n.packages[i].version),
        updates: s
            .updates
            .keys()
            .next_back()
            .map(|v| (*v, path.to_path_buf()))
            .into_iter()
            .collect(),
        dlc,
        app_flag: None,
        has_base: s.base.is_some(),
        notes: Vec::new(),
    };
    if a.base_package.is_some() {
        return reconstruct_nup(input, a);
    }
    if a.update_inputs.is_empty() {
        return Ok(input);
    }
    let key_text = generic_keys(a)?;
    let mut packages = Vec::new();
    for path in &a.update_inputs {
        for package in generic_package_paths(path)? {
            let metas = generic_contents(&key_text, &package)?;
            packages.push((package, GenericRole::Update, metas));
        }
    }
    merge_package_updates(input, packages)
}

/// Finds packages for generic builds with metadata-driven selection.
/// The title-specific discovery and filename compatibility rules stay unchanged.
fn discover_primary(a: &Args) -> Result<Inputs, String> {
    let want = a.title_id.as_deref().map(parse_title).transpose()?;
    let path = a.nup.as_deref().unwrap_or(&a.nsp_dir);
    if a.nup.is_some() || (path.is_file() && nxb_extract::nup::is_nup(path)) {
        return generic_nup(path, a, want, false);
    }
    if path.is_dir() && files_with(path, &SUFFIXES).is_empty() {
        let nups = files_with(path, &["nup"]);
        match nups.as_slice() {
            [one] => return generic_nup(one, a, want, false),
            [] => {}
            _ => {
                return Err(format!(
                    "more than one NUP in {}; give the .nup file",
                    path.display()
                ));
            }
        }
    }
    if a.base_package.is_some() {
        return Err("--base-package requires a NUP primary input".into());
    }
    let key_text = generic_keys(a)?;
    let mut packages = Vec::new();
    for (path, role) in std::iter::once((path, GenericRole::Input)).chain(
        a.update_inputs
            .iter()
            .map(|p| (p.as_path(), GenericRole::Update)),
    ) {
        for p in generic_package_paths(path)? {
            let metas = generic_contents(&key_text, &p)?;
            packages.push((p, role, metas));
        }
    }
    packages.extend(external_dlc(a, Some(key_text))?);
    select_generic(packages, want)
}

fn reconstruct_nup(original: Inputs, a: &Args) -> Result<Inputs, String> {
    let backing = a
        .base_package
        .as_deref()
        .ok_or("--base-package is required")?;
    if a.update_inputs.is_empty() {
        return Err("--base-package requires an explicit encrypted --update package".into());
    }
    if !backing.is_file() || nxb_extract::nup::is_nup(backing) {
        return Err(
            "--base-package must name an encrypted package file, not a NUP or directory".into(),
        );
    }
    let mut args = a.clone();
    args.nup = None;
    args.nsp_dir = backing.to_path_buf();
    args.base_package = None;
    args.title_id = Some(format!("{:016X}", original.app));
    args.aoc.clear();
    let rebuilt = discover_primary(&args)?;
    validate_reconstruction(original, rebuilt)
}

fn validate_reconstruction(original: Inputs, mut rebuilt: Inputs) -> Result<Inputs, String> {
    if rebuilt.app != original.app {
        return Err("--base-package belongs to another application".into());
    }
    if original.has_base && rebuilt.base_version != original.base_version {
        return Err(
            "--base-package Application version does not match the NUP base version".into(),
        );
    }
    let original_version = original
        .updates
        .last()
        .map_or(original.base_version, |u| u.0);
    let rebuilt_version = rebuilt.updates.last().map_or(rebuilt.base_version, |u| u.0);
    if rebuilt_version < original_version || rebuilt_version < rebuilt.base_version {
        return Err("--base-package and --update would downgrade the NUP; supply an update at least as new as its selected version".into());
    }
    rebuilt.dlc.extend(original.dlc);
    rebuilt.dlc = merge_nup_dlc(rebuilt.app, rebuilt.dlc, Vec::new())?;
    Ok(rebuilt)
}

fn select_standalone_updates(
    mut selected: Inputs,
    candidates: Vec<Inputs>,
) -> Result<Inputs, String> {
    let mut dlc = selected.dlc.clone();
    for candidate in candidates {
        if candidate.app != selected.app {
            return Err("external NUP update belongs to another application".into());
        }
        dlc.extend(candidate.dlc.iter().cloned());
        let effective = |input: &Inputs| {
            input
                .updates
                .last()
                .cloned()
                .unwrap_or((input.base_version, input.base.clone()))
        };
        let (version, path) = effective(&candidate);
        let (current, current_path) = effective(&selected);
        if version > current || (version == current && path < current_path) {
            selected = candidate;
        }
    }
    selected.dlc = merge_nup_dlc(selected.app, dlc, Vec::new())?;
    Ok(selected)
}

/// Finds packages and complete standalone NUP updates for generic builds.
pub(crate) fn discover_generic(a: &Args) -> Result<Inputs, String> {
    let mut primary = a.clone();
    primary.update_inputs.clear();
    let mut nups = Vec::new();
    for input in &a.update_inputs {
        for path in mixed_package_paths(input)? {
            if nxb_extract::nup::is_nup(&path) {
                nups.push(path);
            } else {
                primary.update_inputs.push(path);
            }
        }
    }
    let original = discover_primary(&primary)?;
    let mut candidates = Vec::new();
    nups.sort();
    nups.dedup();
    for path in nups {
        candidates.push(generic_nup(
            &path,
            &Args::default(),
            Some(original.app),
            true,
        )?);
    }
    select_standalone_updates(original, candidates)
}

#[cfg(test)]
mod tests;
