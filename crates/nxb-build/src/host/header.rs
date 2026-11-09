//! `game.py`'s `header_one`: the host's `game_profile.h` of one profile (the macros the app binary settings read), with
//! the same checks and the same text.

use super::profile::Game;
use super::pyfmt::{hex, int0, int16, splitlines};
use std::path::Path;

fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn alnum(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn one_of(
    g: &Game,
    key: &str,
    default: &str,
    allowed: &[&str],
    what: &str,
) -> Result<String, String> {
    let v = g.get_or(key, default);
    if allowed.contains(&v.as_str()) {
        Ok(v)
    } else {
        Err(format!("{}: {key} must be {what}: {v}", g.id))
    }
}

fn on_off(g: &Game, key: &str, default: &str) -> Result<bool, String> {
    Ok(one_of(g, key, default, &["on", "off"], "on or off")? == "on")
}

/// `game.NXKIT_SERVICES`: nxkit's services in the order of `nxtitle::Service::ALL`, one bit each in `DM_GAME_NXKIT_SERVICES`.
pub const NXKIT_SERVICES: [&str; 9] = [
    "fs", "input", "audio", "video", "system", "offline", "pacing", "kernel", "gpu",
];

/// The `nxkit_services` key as a bit mask: a comma list of [`NXKIT_SERVICES`], each at most once.
fn nxkit_mask(g: &Game) -> Result<i128, String> {
    let raw = g.get_or("nxkit_services", "");
    let names: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .collect();
    let mut mask = 0i128;
    for w in &names {
        match NXKIT_SERVICES.iter().position(|s| s == w) {
            Some(i) if mask & (1 << i) == 0 => mask |= 1 << i,
            _ => {
                return Err(format!(
                    "{}: nxkit_services takes each of {} at most once, comma-separated: {raw}",
                    g.id,
                    NXKIT_SERVICES.join(", ")
                ));
            }
        }
    }
    Ok(mask)
}

/// The place strings of the NDSR packs in the shader map.
const NDSR_PACKS: [&str; 3] = [
    "/agl_ndsr.pck#",
    "/agl_ndsr_vr.pck#",
    "/agl_ndsr_vr_light.pck#",
];

/// `game.upscaler_keys`: the compute shaders of the three NDSR packs (`agl_ndsr.pck`, `agl_ndsr_vr.pck`,
/// `agl_ndsr_vr_light.pck`) in the shader map, sorted 16-digit keys.
pub fn upscaler_keys(g: &Game) -> Result<Vec<String>, String> {
    let p = g.table("shader_map.tsv");
    if !p.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut keys = std::collections::BTreeSet::new();
    for line in splitlines(&text) {
        let c: Vec<&str> = line.split('\t').collect();
        if c.len() >= 5
            && c[2] == "compute"
            && NDSR_PACKS.iter().any(|p| c[4].contains(p))
            && c[3].len() == 16
            && c[3]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            keys.insert(c[3].to_string());
        }
    }
    Ok(keys.into_iter().collect())
}

/// `shader_key_skip`: `<offset>+<length>` inside the control section, clear of the code area.
fn key_skip(g: &Game) -> Result<(i128, i128), String> {
    let sks = g.get_or("shader_key_skip", "");
    if sks.is_empty() {
        return Ok((0, 0));
    }
    let num = |s: &str| {
        let hexform =
            s.len() > 2 && s.starts_with("0x") && s[2..].bytes().all(|b| b.is_ascii_hexdigit());
        let dec = !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        (hexform || dec).then(|| int0(s))
    };
    let bad = || {
        format!(
            "{}: shader_key_skip takes <offset>+<length> within the first 0x880 bytes of the control section, past its header (+0x30) and clear of +0x700..+0x75f: {sks}",
            g.id
        )
    };
    let (a, b) = sks.split_once('+').ok_or_else(bad)?;
    let (Some(o), Some(l)) = (num(a), num(b)) else {
        return Err(bad());
    };
    let (o, l) = (o?, l?);
    if !(0x30 <= o) || !(0 < l) || o + l > 0x880 || (o < 0x760 && o + l > 0x700) {
        return Err(bad());
    }
    Ok((o, l))
}

/// The header text of one profile. `here` is the engine's `dm/games` (the comment names a profile there by its
/// repository path, any other by its folder).
pub fn header_one(g: &Game, here: &Path) -> Result<String, String> {
    let id = &g.id;
    let mods = g.words("modules")?;
    let builds = g.builds()?;
    let field =
        |b: &[(String, String)], k: &str| b.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone());
    let vers = builds
        .iter()
        .filter_map(|(_, b)| {
            let main = field(b, "main")?;
            Some(format!(
                "{{{}, {}}}",
                q(&main),
                q(&field(b, "display").unwrap_or_default())
            ))
        })
        .collect::<Vec<_>>()
        .join(", ");
    let pi = one_of(
        g,
        "present_interval",
        "game",
        &["game", "display"],
        "game or display",
    )?;
    let ft = one_of(g, "fps_target", "0", &["0", "30", "60"], "0, 30 or 60")?;
    let fm = one_of(g, "fps_max", "60", &["60", "120"], "60 or 120")?;
    let fh = on_off(g, "fps_hold", "on")?;
    let utp = g.get_or("ue_texture_pool", "0");
    if utp.is_empty() || !utp.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "{id}: ue_texture_pool must be megabytes (0 = the game's own pool): {utp}"
        ));
    }
    let ufc = g.get_or("ue_fps_cvar", "t.MaxFPS");
    let ufc_ok = ufc.len() <= 31
        && ufc.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
        && ufc
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.');
    if !ufc_ok {
        return Err(format!(
            "{id}: ue_fps_cvar must be a console variable name: {ufc}"
        ));
    }
    let ps = on_off(g, "present_snap", "off")?;
    let p4 = on_off(g, "pso_mtl4", "off")?;
    let pg = g.words_or("pso_gpu", "");
    if !pg.is_empty()
        && (pg.len() > 4
            || pg.iter().any(|w| {
                w == "none" || !alnum(w.strip_prefix("applegpu_").unwrap_or(w)) && !alnum(w)
            }))
    {
        return Err(format!(
            "{id}: pso_gpu takes 1-4 whitespace-separated GPU architectures: {pg:?}"
        ));
    }
    let et = on_off(g, "enc_thread", "off")?;
    let cp = on_off(g, "core_preempt", "off")?;
    let aba = on_off(g, "alloc_big_align", "off")?;
    let sf = one_of(g, "shared_font", "bfttf", &["bfttf", "ttf"], "bfttf or ttf")?;
    let pl = one_of(
        g,
        "pad_layout",
        "simple",
        &["simple", "full", "touch"],
        "simple, full or touch",
    )?;
    let (sko, skl) = key_skip(g)?;
    let rcd = on_off(g, "render_cond", "off")?;
    let tfu = on_off(g, "tex_flush_upload", "off")?;
    let tdu = g.get_or("tex_dirty_upload", "off");
    if !(tdu == "on" || tdu == "off") || (tdu == "on" && !tfu) {
        return Err(format!(
            "{id}: tex_dirty_upload must be on or off, and needs tex_flush_upload on: {tdu}"
        ));
    }
    let tds = on_off(g, "tex_dyn_slots", "off")?;
    let vcr = on_off(g, "vconv_range", "off")?;
    let mml = on_off(g, "mem_map_limit", "off")?;
    let ups = upscaler_keys(g)?;
    let nxs = nxkit_mask(g)?;
    let dbr = one_of(
        g,
        "draw_barriers",
        "full",
        &["full", "game"],
        "full or game",
    )?;
    let gts = one_of(
        g,
        "gpu_timestamps",
        "submission",
        &["submission", "segment"],
        "submission or segment",
    )?;
    let om = one_of(
        g,
        "op_mode",
        "console",
        &["console", "handheld"],
        "console or handheld",
    )?;
    if pl == "touch" && om != "handheld" {
        return Err(format!(
            "{id}: pad_layout touch needs op_mode handheld (the touch screen reaches the game only in handheld mode)"
        ));
    }
    let pm = one_of(
        g,
        "perf_mode",
        "follow",
        &["follow", "boost"],
        "follow or boost",
    )?;
    one_of(g, "depth_mode", "gl", &["gl", "zero"], "gl or zero")?;
    let api = g.get_or("device_api", "55.15");
    let api_parts: Option<(&str, &str)> = api.split_once('.').filter(|(a, b)| {
        [a, b]
            .iter()
            .all(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
    });
    let Some((api_major, api_minor)) = api_parts else {
        return Err(format!("{id}: device_api must be major.minor: {api}"));
    };
    let here = std::fs::canonicalize(here).unwrap_or_else(|_| here.to_path_buf());
    let src = if g.dir.parent() == Some(here.as_path()) {
        format!("dm/games/{id}")
    } else {
        g.dir.display().to_string()
    };
    let b = |v: bool| u8::from(v).to_string();
    let int_or = |k: &str, d: &str| int0(&g.get_or(k, d));
    let screen_aspect = match g.get_or("screen_aspect", "off").as_str() {
        "off" => 0,
        "on" => 1,
        v => return Err(format!("{id}: screen_aspect must be off or on: {v}")),
    };
    let build_modules = g
        .build_modules()?
        .iter()
        .map(|(bid, ms)| format!("{{{}, {}}}", q(bid), q(&ms.join(" "))))
        .collect::<Vec<_>>()
        .join(", ");
    let mut l: Vec<String> = vec![
        format!("// Generated by dm/games/game.py from {src}/game.tsv; do not edit"),
        "#pragma once".into(),
        format!("#define DM_GAME_ID {}", q(id)),
        format!("#define DM_GAME_NAME {}", q(&g.get("name")?)),
        format!(
            "#define DM_GAME_PROGRAM_ID 0x{:016X}ull",
            int16(&g.get("program_id")?)?
        ),
        format!(
            "#define DM_GAME_MODULES {{{}}}",
            mods.iter().map(|m| q(m)).collect::<Vec<_>>().join(", ")
        ),
        format!("#define DM_GAME_NMOD {}", mods.len()),
        format!(
            "#define DM_GAME_BUILD_MODULES {}",
            if build_modules.is_empty() {
                "{0}".to_string()
            } else {
                build_modules
            }
        ),
        format!(
            "#define DM_GAME_ROMFS_MARKER {}",
            q(&g.get("romfs_marker")?)
        ),
        format!(
            "#define DM_GAME_MEM_RESERVE {}ull",
            hex(g.int("mem_reserve")?)
        ),
        format!("#define DM_GAME_MEM_ALIAS {}ull", hex(g.int("mem_alias")?)),
        format!("#define DM_GAME_MEM_STACK {}ull", hex(g.int("mem_stack")?)),
        format!("#define DM_GAME_APP_POOL {}ull", hex(g.int("app_pool")?)),
        format!(
            "#define DM_GAME_MEM_USED {}ull",
            hex(int_or("mem_used", "0x4000000")?)
        ),
        format!(
            "#define DM_GAME_SYS_RESOURCE {}ull",
            hex(int_or("sys_resource", "0x2000000")?)
        ),
        format!("#define DM_GAME_ARGS {}", q(&g.get_or("game_args", ""))),
        format!("#define DM_GAME_ENV {}", q(&g.get_or("game_env", ""))),
        format!(
            "#define DM_GAME_LAUNCH_PARAM {}",
            q(&g.get_or("launch_param", ""))
        ),
        format!("#define DM_GAME_MEM_MAP_LIMIT {}", b(mml)),
        format!(
            "#define DM_GAME_STDALLOC_CAP {}ull",
            hex(int_or("stdalloc_cap", "0")?)
        ),
        format!("#define DM_GAME_SCREEN_ASPECT {screen_aspect}"),
        format!("#define DM_GAME_DISPLAY_VERSIONS {vers}"),
        format!("#define DM_GAME_PRESENT_DISPLAY {}", b(pi == "display")),
        format!("#define DM_GAME_CORE_PREEMPT {}", b(cp)),
        format!("#define DM_GAME_ALLOC_BIG_ALIGN {}", b(aba)),
        format!("#define DM_GAME_FPS_TARGET {ft}"),
        format!("#define DM_GAME_FPS_MAX {fm}"),
        format!("#define DM_GAME_FPS_HOLD {}", b(fh)),
        format!("#define DM_GAME_UE_TEXTURE_POOL {}", int0(&utp)?),
        format!("#define DM_GAME_UE_FPS_CVAR {}", q(&ufc)),
        format!("#define DM_GAME_PRESENT_SNAP {}", b(ps)),
        format!("#define DM_GAME_PSO_MTL4 {}", b(p4)),
        format!("#define DM_GAME_ENC_THREAD {}", b(et)),
    ];
    if !g.get_or("shared_font", "").is_empty() {
        l.push(format!(
            "#define DM_GAME_SHARED_FONT_TTF {}",
            b(sf == "ttf")
        ));
    }
    let pad = ["simple", "full", "touch"]
        .iter()
        .position(|x| *x == pl)
        .unwrap_or(0);
    l.extend([
        format!("#define DM_GAME_PAD_LAYOUT {pad}"),
        format!("#define DM_GAME_OPMODE {}", b(om == "console")),
        format!("#define DM_GAME_PERF_BOOST {}", b(pm == "boost")),
        format!("#define DM_GAME_TEX_FLUSH_UPLOAD {}", b(tfu)),
        format!("#define DM_GAME_SHADER_KEY_SKIP_OFF {}", hex(sko)),
        format!("#define DM_GAME_SHADER_KEY_SKIP_LEN {}", hex(skl)),
        format!("#define DM_GAME_RENDER_COND {}", b(rcd)),
        format!("#define DM_GAME_TEX_DIRTY_UPLOAD {}", b(tdu == "on")),
        format!("#define DM_GAME_TEX_DYN_SLOTS {}", b(tds)),
        format!("#define DM_GAME_GPU_TS_SEGMENT {}", b(gts == "segment")),
        format!("#define DM_GAME_DRAW_BARRIERS_GAME {}", b(dbr == "game")),
        format!("#define DM_GAME_UPSCALER_KEYS {}", q(&ups.join(","))),
        format!("#define DM_GAME_UPSCALER_N {}", ups.len()),
        format!("#define DM_GAME_VCONV_RANGE {}", b(vcr)),
        format!("#define DM_GAME_NXKIT_SERVICES {}ull", hex(nxs)),
    ]);
    if !g.get_or("device_api", "").is_empty() {
        l.push(format!("#define DM_GAME_NVN_API_MAJOR {api_major}"));
        l.push(format!("#define DM_GAME_NVN_API_MINOR {api_minor}"));
    }
    l.push(String::new());
    Ok(l.join("\n"))
}

/// `re.findall(r'^#define (DM_GAME_\w+) (.*)$', header, re.M)` as a `dict` (a later line wins).
pub fn macros(header: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in header.split('\n') {
        let Some(rest) = line.strip_prefix("#define DM_GAME_") else {
            continue;
        };
        let n = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        if let Some(value) = rest[n..].strip_prefix(' ') {
            super::profile::dict_set(
                &mut out,
                &format!("DM_GAME_{}", &rest[..n]),
                value.to_string(),
            );
        }
    }
    out
}
