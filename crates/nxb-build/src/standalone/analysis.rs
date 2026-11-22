//! Target-independent facts and rewritten modules from the input, without a title catalog. The `sdk` module gets the
//! SDK-wide rules, which match by function fingerprint and not by build ID.
use super::{auto_shaders, elf, extract, graphics, layout, manifest, modules, npdm, nvn};
use nxtitle::GraphicsApi;
use std::collections::BTreeSet;

/// The SDK-wide rules of the title modules, for the `sdk` module of any game (`sdk_rules.toml`).
const SDK_RULES: &str = include_str!("sdk_rules.toml");

pub(super) struct Analysis {
    pub modules: Vec<(modules::Built, u64)>,
    pub thread: npdm::MainThread,
    /// The API the modules ask for by name. A game that names none is taken for NVN.
    pub graphics: GraphicsApi,
    /// The engine family found from the modules, as its id, and what it gives the manifest.
    pub engine: Option<&'static str>,
    limits: Vec<nxtitle::LimitRule>,
    render_args: Option<nxtitle::RenderArgs>,
    /// The modules left out because the host's Vulkan layer replaces them (the loader and the driver).
    pub replaced: Vec<String>,
    settings: nxtitle::Settings,
}

fn ordered(input: &[(String, Vec<u8>)]) -> Result<Vec<&(String, Vec<u8>)>, String> {
    let mut seen = BTreeSet::new();
    let mut selected = Vec::new();
    for m in input {
        if !seen.insert(m.0.as_str()) {
            return Err(format!("duplicate module {}", m.0));
        }
        let rank = match m.0.as_str() {
            "rtld" => continue,
            "main" => 0,
            "sdk" => 11,
            n if n.len() == 7 && n.starts_with("subsdk") && n.as_bytes()[6].is_ascii_digit() => {
                1 + n.as_bytes()[6] - b'0'
            }
            n => {
                return Err(format!(
                    "unsupported module name {n}: load order is unknown"
                ));
            }
        };
        selected.push((rank, m));
    }
    if !seen.contains("main") {
        return Err("ExeFS has no main NSO module".into());
    }
    selected.sort_by_key(|(rank, _)| *rank);
    Ok(selected.into_iter().map(|(_, m)| m).collect())
}

/// The graphics API of the modules, from the names in their read-only data, and for Vulkan the modules that the host's
/// layer replaces: a module besides `main` and `sdk` that exports `vk*` functions is the Vulkan loader or the driver.
fn api_of(selected: &[&(String, Vec<u8>)]) -> Result<(GraphicsApi, Vec<String>), String> {
    let mut ro = Vec::new();
    let mut vulkan_modules = Vec::new();
    for (name, data) in selected {
        let m = nxb_rewrite::nso::load(data, name).map_err(|e| format!("module {name}: {e:?}"))?;
        let (off, size) = m.segments[1];
        ro.push(
            m.image
                .get(off..off.saturating_add(size))
                .unwrap_or_default()
                .to_vec(),
        );
        let exports_vk = elf::symbols(&m.image)
            .is_ok_and(|s| s.exports.iter().any(|e| e.name.starts_with("vk")));
        if exports_vk && !matches!(name.as_str(), "main" | "sdk") {
            vulkan_modules.push(name.clone());
        }
    }
    let api = graphics::detect(ro.iter().map(Vec::as_slice)).unwrap_or(GraphicsApi::Nvn);
    let replaced = if api == GraphicsApi::Vulkan {
        vulkan_modules
    } else {
        vec![]
    };
    Ok((api, replaced))
}

pub(super) fn analyze(game: &extract::Extracted) -> Result<Analysis, String> {
    let thread = npdm::main_thread(&game.npdm)?;
    let mut selected = ordered(&game.modules)?;
    let (graphics, replaced) = api_of(&selected)?;
    selected.retain(|(name, _)| !replaced.contains(name));
    let ids = selected
        .iter()
        .map(|(name, data)| {
            auto_shaders::check_nso_size(data)?;
            // The rewrite loads and validates the complete NSO below. Do not decompress it twice.
            let id = data
                .get(0x40..0x50)
                .ok_or_else(|| format!("{name}: truncated NSO build ID"))?;
            Ok((name.as_str(), nxb_model::to_hex(id)))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let order = ids
        .iter()
        .map(|(name, id)| (*name, id.as_str()))
        .collect::<Vec<_>>();
    let built = modules::rewrite_with_loader(
        &order,
        "generic",
        SDK_RULES,
        &[],
        |name| {
            selected
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, bytes)| bytes.clone())
                .ok_or_else(|| format!("missing module {name}"))
        },
        |_| {},
        |_, _, _| {},
    )?;
    let bases = layout::bases(&built.iter().map(|m| m.shape.clone()).collect::<Vec<_>>());
    let mut settings = nxtitle::Settings::DEFAULT;
    let resource = game.npdm.get(0x14..0x18).ok_or("truncated NPDM")?;
    // These are host defaults, not game-specific requirements inferred from the binary.
    settings.memory = nxtitle::Memory {
        reserve: 4 << 30,
        alias: 256 << 20,
        stack: 256 << 20,
        app_pool: 3 << 30,
        used: 64 << 20,
        system_resource: u32::from_le_bytes(
            resource.try_into().map_err(|_| "invalid NPDM resource")?,
        ) as u64,
        map_limit: true,
        stdalloc_cap: 0,
        alloc_big_align: false,
    };
    // The engine: the library `UnrealEngine` is named when the rewriter registered an engine console variable in `main`.
    let mut facts = nxtitle::TitleFacts::new(nxtitle::TitleId(game.title_id));
    facts.version = Some(game.version);
    facts.modules = built.iter().map(|m| m.name.clone()).collect();
    if built
        .iter()
        .any(|m| m.name == "main" && m.fixups.contains("UE_CVAR:"))
    {
        facts.libraries.push("UnrealEngine".into());
    }
    let resolved = nxtitles::catalog()?.resolve(&facts);
    let engine = resolved.engine;
    Ok(Analysis {
        engine: engine.map(|f| f.id()),
        limits: engine
            .map(|f| f.limits().iter().map(|l| l.value).collect())
            .unwrap_or_default(),
        render_args: engine.and_then(|f| f.render_args().map(|a| a.value)),
        modules: built.into_iter().zip(bases).collect(),
        thread,
        graphics,
        replaced,
        settings,
    })
}

impl Analysis {
    pub(super) fn manifest(&self, game: &extract::Extracted) -> String {
        manifest::game_json(&manifest::Manifest {
            title_id: game.title_id,
            name: &game.name,
            version: game.version,
            version_name: game.version_name.clone(),
            modules: &self.modules,
            // Enable the available shared services. This is not a compatibility verdict.
            services: nxtitle::Service::ALL.iter().map(|s| s.id()).collect(),
            graphics: self.graphics.id(),
            // Only a game on NVN fetches NVN functions
            nvn_functions: if self.graphics == GraphicsApi::Nvn {
                nvn::names(&self.modules)
            } else {
                vec![]
            },
            patches: false,
            main_thread: self.thread,
            aoc: &game.aoc,
            settings: self.settings,
            shared_font_decode: false,
            sf_services: vec![],
            limits: self.limits.clone(),
            render_args: self.render_args,
            temporal_upscale: None,
            vertex_stream_null_bind: false,
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_sdk_rules_parse_for_the_generic_scope() {
        let t = nxb_rewrite::patches::PatchTable::from_toml(&[(
            "generic.toml".to_owned(),
            SDK_RULES.to_owned(),
        )])
        .unwrap();
        assert_eq!(t.rules["generic"]["sdk"].len(), 12);
        assert!(!t.rules["generic"].contains_key("main"));
    }

    use super::*;
    fn u32b(b: &mut [u8], at: usize, n: u32) {
        b[at..at + 4].copy_from_slice(&n.to_le_bytes());
    }
    fn u64b(b: &mut [u8], at: usize, n: u64) {
        b[at..at + 8].copy_from_slice(&n.to_le_bytes());
    }
    fn nso() -> Vec<u8> {
        let mut b = vec![0; 0x5100];
        b[..4].copy_from_slice(b"NSO0");
        b[0x40..0x50].fill(1);
        for (i, (offset, size)) in [(0, 0x1000), (0x1000, 0x3000), (0x4000, 0x1000)]
            .into_iter()
            .enumerate()
        {
            u32b(&mut b, 0x10 + i * 16, 0x100 + offset);
            u32b(&mut b, 0x14 + i * 16, offset);
            u32b(&mut b, 0x18 + i * 16, size);
            u32b(&mut b, 0x60 + i * 4, size);
        }
        u32b(&mut b, 0x3c, 0x1000);
        for at in (0x100..0x1100).step_by(4) {
            u32b(&mut b, at, 0xd503201f);
        }
        for (at, n) in [
            (0x100, 0x14000010),
            (0x104, 0x1000),
            (0x140, 0xd4000541),
            (0x144, 0xd65f03c0),
            (0x1104, 0x40),
            (0x1110, 0x180),
            (0x1304, 1),
        ] {
            u32b(&mut b, at, n);
        }
        b[0x1100..0x1104].copy_from_slice(b"MOD0");
        b[0x1280..0x1284].copy_from_slice(&[1, 3, 3, 3]);
        for (i, (tag, value)) in [(4, 0x1200), (5, 0x1300), (6, 0x1240), (0, 0)]
            .into_iter()
            .enumerate()
        {
            u64b(&mut b, 0x1140 + i * 16, tag);
            u64b(&mut b, 0x1148 + i * 16, value);
        }
        b
    }

    #[test]
    fn synthetic_input_derives_manifest_without_catalog_or_patches() {
        let mut npdm = vec![0; 0x80];
        npdm[..4].copy_from_slice(b"META");
        npdm[0xe] = 44;
        u32b(&mut npdm, 0x1c, 0x100000);
        u32b(&mut npdm, 0x14, 0x200000);
        let mut game = extract::Extracted {
            title_id: 0x010000000000abcd,
            name: "Synthetic".into(),
            version: 0,
            version_name: "1.0".into(),
            npdm,
            modules: vec![("main".into(), nso())],
            files: vec![],
            directories: vec![],
            aoc: vec![],
        };
        let result = analyze(&game).unwrap();
        assert_eq!(result.modules[0].0.build_id, "01".repeat(16));
        assert_eq!(result.thread.stack_size, 0x100000);
        assert_eq!(result.settings.memory.system_resource, 0x200000);
        let json = result.manifest(&game);
        assert!(!json.contains("patches.toml"));
        assert!(json.contains("Synthetic"));
        game.modules[0].1.truncate(0x4f);
        assert!(analyze(&game).is_err());
    }
    #[test]
    fn load_order_is_not_container_order() {
        let input = ["sdk", "subsdk9", "rtld", "main", "subsdk0"].map(|s| (s.into(), vec![]));
        let names = ordered(&input)
            .unwrap()
            .into_iter()
            .map(|m| m.0.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["main", "subsdk0", "subsdk9", "sdk"]);
    }
    #[test]
    fn ambiguous_or_unknown_modules_fail_closed() {
        for names in [
            vec!["main", "main"],
            vec!["sdk"],
            vec!["main", "plugin"],
            vec!["main", "subsdk10"],
        ] {
            let input = names
                .into_iter()
                .map(|s| (s.into(), vec![]))
                .collect::<Vec<_>>();
            assert!(ordered(&input).is_err());
        }
    }
}
