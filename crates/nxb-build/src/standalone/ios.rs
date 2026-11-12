//! Native unsigned single-application iOS producer. This path does not sign or execute game code.
use super::{extract, guest_tail, ipa, layout, link, manifest, modules, npdm, shaders_ios};
use nxb_pyjson::{Value as J, parse};
use std::path::PathBuf;
type Result<T> = std::result::Result<T, String>;
/// Whether the caller selected the native iOS build.
pub fn requested(args: &[String]) -> bool {
    args.iter().any(|a| a == "--platform=ios")
        || args.windows(2).any(|w| w == ["--platform", "ios"])
}
/// Build an unsigned IPA from one NSP or XCI and an SDK-built host app.
pub fn main(args: &[String]) -> i32 {
    let options = match options(args) {
        Ok(options) => options,
        Err(e) => {
            eprintln!("nxb: iOS build: {e}");
            return 2;
        }
    };
    match build(options) {
        Ok(path) => {
            println!("unsigned IPA: {}", path.display());
            0
        }
        Err(e) => {
            eprintln!("nxb: iOS build: {e}");
            1
        }
    }
}
struct Options {
    input: PathBuf,
    keys: PathBuf,
    host: PathBuf,
    out: PathBuf,
}
fn options(args: &[String]) -> Result<Options> {
    let mut input = None;
    let mut opts = std::collections::BTreeMap::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let flag = match arg.as_str() {
            "-k" => Some("keys"),
            "-o" => Some("out"),
            _ => arg.strip_prefix("--"),
        };
        if let Some(flag) = flag {
            let (key, value) = if let Some((k, v)) = flag.split_once('=') {
                (k, v)
            } else {
                (
                    flag,
                    it.next()
                        .ok_or_else(|| format!("missing value for --{flag}"))?
                        .as_str(),
                )
            };
            if !["platform", "keys", "host-app", "out"].contains(&key) {
                return Err(format!("unknown iOS option --{key}"));
            }
            if opts.insert(key, value).is_some() {
                return Err(format!("duplicate --{key}"));
            }
        } else if input.replace(PathBuf::from(arg)).is_some() {
            return Err("supply exactly one NSP or XCI".into());
        }
    }
    let get = |key| {
        opts.get(key)
            .map(|s| PathBuf::from(*s))
            .ok_or_else(|| format!("missing --{key}"))
    };
    if opts.get("platform") != Some(&"ios") {
        return Err("expected --platform ios".into());
    }
    let input = input.ok_or("missing NSP or XCI input")?;
    if !input
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("nsp") || s.eq_ignore_ascii_case("xci"))
    {
        return Err("input must be NSP or XCI".into());
    }
    let out = get("out")?;
    if out.extension().and_then(|s| s.to_str()) != Some("ipa") {
        return Err("--out must end in .ipa".into());
    }
    Ok(Options {
        input,
        keys: get("keys")?,
        host: get("host-app")?,
        out,
    })
}
fn build(opts: Options) -> Result<PathBuf> {
    if opts.out.exists() {
        return Err("output exists, choose a new --out path".into());
    }
    let host = ipa::read_host(&opts.host)?;
    let game = extract::extract(&opts.input, &opts.keys)?;
    let mut files = runfolder(&game)?;
    files.extend(shaders_ios::discover(&game.modules, &game.files)?);
    files.extend(game.files);
    ipa::write(
        &opts.out,
        host,
        files,
        &game.directories,
        game.title_id,
        &game.name,
    )?;
    Ok(opts.out)
}
fn runfolder(game: &extract::Extracted) -> Result<Vec<(String, Vec<u8>)>> {
    // nxkit supplies the loader. Do not run Nintendo's rtld. ExeFS order is not load order.
    let rank = |name: &str| -> Result<u8> {
        match name {
            "main" => Ok(0),
            "sdk" => Ok(11),
            _ if name.len() == 7
                && name.starts_with("subsdk")
                && name.as_bytes()[6].is_ascii_digit() =>
            {
                Ok(1 + name.as_bytes()[6] - b'0')
            }
            _ => Err(format!(
                "unsupported module name {name}, explicit load order is required"
            )),
        }
    };
    let mut selected = game
        .modules
        .iter()
        .filter(|(name, _)| name != "rtld")
        .map(|m| Ok((rank(&m.0)?, m)))
        .collect::<Result<Vec<_>>>()?;
    selected.sort_by_key(|(rank, _)| *rank);
    let ids = selected
        .iter()
        .map(|(_, (name, data))| {
            shaders_ios::check_nso_size(data)?;
            let nso = nxb_rewrite::nso::load(data, name).map_err(|e| format!("{name}: {e:?}"))?;
            Ok((name.as_str(), nxb_model::to_hex(&nso.build_id[..16])))
        })
        .collect::<Result<Vec<_>>>()?;
    let order = ids
        .iter()
        .map(|(name, id)| (*name, id.as_str()))
        .collect::<Vec<_>>();
    let built = modules::rewrite_with_loader(
        &order,
        "ios",
        "scope = \"ios\"",
        |name| {
            game.modules
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, data)| data.clone())
                .ok_or_else(|| format!("missing module {name}"))
        },
        |_| {},
        |_, _, _| {},
    )?;
    let bases = layout::bases(&built.iter().map(|b| b.shape.clone()).collect::<Vec<_>>());
    let mods = built.into_iter().zip(bases).collect::<Vec<_>>();
    let thread = npdm::main_thread(&game.npdm)?;
    let mut settings = nxtitle::Settings::DEFAULT;
    let resource = game.npdm.get(0x14..0x18).ok_or("truncated NPDM")?;
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
    let m = manifest::Manifest {
        title_id: game.title_id,
        name: &game.name,
        version: game.version,
        version_name: game.version_name.clone(),
        modules: &mods,
        services: nxtitle::Service::ALL.iter().map(|s| s.id()).collect(),
        nvn_functions: super::nvn::names(&mods),
        patches: false,
        main_thread: thread,
        aoc: &[],
        settings,
        shared_font_decode: false,
        sf_services: vec![],
        vertex_stream_null_bind: false,
    };
    let mut json = parse(&manifest::game_json(&m)).map_err(|e| format!("manifest: {e:?}"))?;
    json.set("dylib", J::str("Frameworks/nxgame.dylib"));
    let mut image = link::ios_dylib(&mods)?;
    guest_tail::install(&mut image, &json, thread.stack_size as u64)?;
    let mut files = vec![
        ("game.json".into(), nxb_pyjson::dumps(&json).into_bytes()),
        ("main.npdm".into(), game.npdm.clone()),
        ("Frameworks/nxgame.dylib".into(), image),
    ];
    for (m, _) in mods {
        files.push((
            format!("modules/{}.imports.json", m.name),
            manifest::imports_json(&m.symbols.imports).into_bytes(),
        ));
        files.push((
            format!("modules/{}.exports.json", m.name),
            manifest::symbols_json(&m.symbols.exports).into_bytes(),
        ));
        files.push((format!("modules/{}.bin", m.name), m.image));
        files.push((
            format!("modules/{}.fixups.tsv", m.name),
            m.fixups.into_bytes(),
        ));
        files.push((format!("modules/{}.gates.tsv", m.name), m.gates));
    }
    Ok(files)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_ambiguous_arguments() {
        assert_eq!(
            main(&["game.nsp".into(), "--platform".into(), "ios".into()]),
            2
        );
        let args = [
            "a.nsp",
            "--platform",
            "ios",
            "--out",
            "test.ipa",
            "--keys",
            "keys",
            "--host-app",
            "Host.app",
        ]
        .map(String::from);
        assert!(options(&args).is_ok());
        let mut short = args.clone();
        short[3] = "-o".into();
        short[5] = "-k".into();
        assert!(options(&short).is_ok());
        let mut duplicate = short.to_vec();
        duplicate.extend(["--keys".into(), "other.keys".into()]);
        assert!(options(&duplicate).is_err());
        let mut bad = args.to_vec();
        bad.push("b.nsp".into());
        assert!(options(&bad).is_err());
    }
}
