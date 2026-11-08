//! `nn::pl` shared font and `nn::fontll::ScalableFontEngine` take-overs. Source: nxnative `dm/host/hle_sdk16.c`, the
//! `pl` and `fontll` sections.
//!
//! The console fonts are not in the game. The host loads a stand-in TrueType font into guest memory once, behind a
//! `OnceLock`, so a call after the first is a plain load. The standard font comes from `Config::shared_font`, else
//! `standard.ttf` in `Config::font_dir`. The extended font (button icons, shared font type 5) comes from
//! `Config::shared_font_ext`, else `nintendo_ext.ttf` in the font directory, and without it type 5 uses the standard
//! font. A missing font gives a null address.
//!
//! A font in guest memory is the 8-byte BFTTF header `{u32 0x18029a7f, u32 bswap32(size ^ 0x49621806)}` and the TTF.
//! A game reads the font in one of two forms, and `Config::shared_font_decode` tells which. A game that decodes the
//! header (`ScalableFontEngineHelper::Decode`) gets the address of the header. Every other game gets the address of the
//! TTF itself, and `GetSharedFont` and `ScalableFontEngine::LoadFont` always take the TTF.
//!
//! The font engine wrappers call the function of the game through `guest_sym`. A game sizes the work memory of an
//! engine for the console fonts, and the stand-in is far larger, so an engine with a small buffer runs on a host
//! buffer of 0x80000 bytes. `GetPointerToWorkBuffer` still gives the game its own buffer back, which the game frees.

use super::{log, put_bytes, put_i32, put_u32, put_u64};
use crate::config;
use crate::event::guest_alloc;
use crate::seams::guest_sym;
use crate::sys::call_guest;
use nxkit_abi::guest;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

const HEADER: u64 = 8;
const ENGINES: usize = 16;
const WORK_MIN: u32 = 0x80000;

/// A font in guest memory: the address of its header and its length with the header.
#[derive(Clone, Copy, Default)]
struct Font {
    addr: u64,
    len: u64,
}

fn map(path: &Path) -> Option<Font> {
    let data = std::fs::read(path).ok()?;
    let len = data.len() as u64;
    let addr = guest_alloc(len + HEADER);
    if addr == 0 {
        log(&format!(
            "shared font memory allocation failed ({} bytes)",
            len + HEADER
        ));
        return None;
    }
    let size = u32::try_from(len).ok()?;
    let mut head = [0u8; 8];
    head[..4].copy_from_slice(&0x1802_9a7f_u32.to_le_bytes());
    head[4..].copy_from_slice(&(size ^ 0x4962_1806).swap_bytes().to_le_bytes());
    guest::write(addr, &head).ok()?;
    guest::write(addr + HEADER, &data).ok()?;
    Some(Font {
        addr,
        len: len + HEADER,
    })
}

fn first(configured: Option<&PathBuf>, file: &str, fallback: Option<&str>) -> Font {
    let dir = config().font_dir.as_ref().map(|d| d.join(file));
    let fallback = fallback.map(PathBuf::from);
    for path in [configured.cloned(), dir, fallback].into_iter().flatten() {
        if let Some(f) = map(&path) {
            log(&format!(
                "shared font ({file}): {} ({} bytes)",
                path.display(),
                f.len - HEADER
            ));
            return f;
        }
    }
    Font::default()
}

static STANDARD: OnceLock<Font> = OnceLock::new();
static EXTENDED: OnceLock<Font> = OnceLock::new();

fn standard() -> Font {
    *STANDARD.get_or_init(|| {
        let f = first(config().shared_font.as_ref(), "standard.ttf", Some("/System/Library/Fonts/Supplemental/Arial Unicode.ttf"));
        if f.addr == 0 {
            log("cannot open the standard shared font: set Config::shared_font or put standard.ttf in Config::font_dir");
        }
        f
    })
}

fn extended() -> Font {
    *EXTENDED.get_or_init(|| {
        let f = first(config().shared_font_ext.as_ref(), "nintendo_ext.ttf", None);
        if f.addr == 0 {
            log("the extended shared font is missing: button icons draw as a question mark");
        }
        f
    })
}

fn font(kind: u32) -> Font {
    if kind == 5 {
        let e = extended();
        if e.addr != 0 {
            return e;
        }
    }
    standard()
}

/// How far the address of a font moves past the header: 0 for a game that decodes the header, else 8.
fn skip() -> u64 {
    if config().shared_font_decode {
        0
    } else {
        HEADER
    }
}

/// `GetSharedFontLoadState(SharedFontType)`: loaded when the font exists.
extern "C" fn load_state(kind: u32) -> u32 {
    u32::from(font(kind).addr != 0)
}

/// `GetSharedFontAddress(SharedFontType)`.
extern "C" fn address(kind: u32) -> u64 {
    let f = font(kind);
    if f.addr == 0 { 0 } else { f.addr + skip() }
}

/// `GetSharedFontSize(SharedFontType)`.
extern "C" fn size(kind: u32) -> u64 {
    let f = font(kind);
    if f.addr == 0 { 0 } else { f.len - skip() }
}

/// `GetSharedFont(int*, SharedFontData*, int, LanguageCode)`: the six types in order, each entry
/// `{u32 type, u32 pad, u64 address, u64 size}` with the TTF itself.
extern "C" fn shared_fonts(count: u64, out: u64, max: u32) -> u32 {
    let max = max as i32;
    let mut n = 0i32;
    for kind in 0..6u32 {
        let f = font(kind);
        if f.addr == 0 {
            continue;
        }
        if n < max {
            let at = out + 0x18 * n as u64;
            put_u32(at, kind);
            put_u32(at + 4, 0);
            put_u64(at + 8, f.addr + HEADER);
            put_u64(at + 16, f.len - HEADER);
        }
        n += 1;
    }
    put_i32(count, n);
    u32::from(n > 0)
}

/// `GetBuiltInFontData(BuiltInFontData*, BuiltInFontType, bool)`: `{u8 type, u8 flag, pad[6], u64 address, u64 size}`.
extern "C" fn built_in(out: u64, kind: u32) -> u32 {
    let f = font(kind);
    if f.addr == 0 {
        return 0;
    }
    let mut b = [0u8; 24];
    b[0] = kind as u8;
    b[8..16].copy_from_slice(&(f.addr + skip()).to_le_bytes());
    b[16..24].copy_from_slice(&(f.len - skip()).to_le_bytes());
    put_bytes(out, &b);
    1
}

/// `BeginVrMode()`: the result of a refused stereo vision permission.
extern "C" fn begin_vr() -> u32 {
    log("pl::BeginVrMode: the host does not enter VR");
    0xd08e
}

/// The font engine takes the TTF itself, so the base of a host font gets the header cut off.
fn payload(data: u64) -> u64 {
    let known = [STANDARD.get(), EXTENDED.get()];
    if known
        .iter()
        .flatten()
        .any(|f| f.addr != 0 && f.addr == data)
    {
        data + HEADER
    } else {
        data
    }
}

static LOAD_FONT: OnceLock<Option<u64>> = OnceLock::new();
static INITIALIZE: OnceLock<Option<u64>> = OnceLock::new();
static WORK_BUFFER: OnceLock<Option<u64>> = OnceLock::new();

fn original(cell: &OnceLock<Option<u64>>, name: &str) -> u64 {
    match cell.get_or_init(|| guest_sym(name)) {
        Some(a) => *a,
        None => super::fatal(&format!("{name} not found in the game")),
    }
}

/// `ScalableFontEngine::LoadFont(char*, const void*, unsigned, unsigned)`: the game's function with a TTF address.
extern "C" fn load_font(this: u64, name: u64, data: u64, index: u32, capacity: u32) -> u32 {
    let f = original(
        &LOAD_FONT,
        "_ZN2nn6fontll18ScalableFontEngine8LoadFontEPcPKvjj",
    );
    call_guest(
        f,
        [
            this,
            name,
            payload(data),
            u64::from(index),
            u64::from(capacity),
            0,
        ],
    ) as u32
}

struct Engine {
    this: AtomicU64,
    game: AtomicU64,
    host: AtomicU64,
}

static ENGINES_TABLE: [Engine; ENGINES] = [const {
    Engine {
        this: AtomicU64::new(0),
        game: AtomicU64::new(0),
        host: AtomicU64::new(0),
    }
}; ENGINES];

/// Finds the entry of an engine, or claims a free one when `claim` is set.
fn engine(this: u64, claim: bool) -> Option<&'static Engine> {
    for e in &ENGINES_TABLE {
        match e.this.load(Ordering::Acquire) {
            t if t == this => return Some(e),
            0 if claim => {
                match e
                    .this
                    .compare_exchange(0, this, Ordering::AcqRel, Ordering::Acquire)
                {
                    Ok(_) => return Some(e),
                    Err(t) if t == this => return Some(e),
                    Err(_) => {}
                }
            }
            0 => return None,
            _ => {}
        }
    }
    None
}

/// `ScalableFontEngine::Initialize(void*, unsigned)`: a small work buffer of the game is replaced by host memory.
extern "C" fn initialize(this: u64, buf: u64, len: u32) {
    let f = original(
        &INITIALIZE,
        "_ZN2nn6fontll18ScalableFontEngine10InitializeEPvj",
    );
    let (mut buf, mut len) = (buf, len);
    if len < WORK_MIN {
        match engine(this, true) {
            Some(e) => {
                let mut host = e.host.load(Ordering::Acquire);
                if host == 0 {
                    host = guest_alloc(u64::from(WORK_MIN));
                    if host == 0 {
                        super::fatal("font engine work memory");
                    }
                    e.host.store(host, Ordering::Release);
                }
                e.game.store(buf, Ordering::Release);
                buf = host;
                len = WORK_MIN;
            }
            None => log(&format!(
                "more than {ENGINES} font engines with a small work buffer, this one keeps the game's"
            )),
        }
    }
    call_guest(f, [this, buf, u64::from(len), 0, 0, 0]);
}

/// `ScalableFontEngine::GetPointerToWorkBuffer()`: the game's own buffer, also when the engine runs on host memory.
extern "C" fn work_buffer(this: u64) -> u64 {
    if let Some(e) = engine(this, false) {
        let game = e.game.load(Ordering::Acquire);
        if game != 0 {
            return game;
        }
    }
    call_guest(
        original(
            &WORK_BUFFER,
            "_ZN2nn6fontll18ScalableFontEngine22GetPointerToWorkBufferEv",
        ),
        [this, 0, 0, 0, 0, 0],
    )
}

table! {
    "_ZN2nn2pl22GetSharedFontLoadStateENS0_14SharedFontTypeE" => load_state,
    "_ZN2nn2pl20GetSharedFontAddressENS0_14SharedFontTypeE" => address,
    "_ZN2nn2pl17RequestLoadNxFontEPvm" => super::nothing,
    "_ZN2nn2pl17GetSharedFontSizeENS0_14SharedFontTypeE" => size,
    "_ZN2nn2pl13GetSharedFontEPiPNS0_14SharedFontDataEiNS_8settings12LanguageCodeE" => shared_fonts,
    "_ZN2nn2pl18GetBuiltInFontDataEPNS0_17BuiltInFontDataNxENS0_17BuiltInFontTypeNxEb" => built_in,
    "_ZN2nn2pl18GetBuiltInFontDataEPNS0_20BuiltInFontDataOunceENS0_20BuiltInFontTypeOunceEb" => built_in,
    "_ZN2nn2pl16IsRunningOnOunceEv" => super::zero,
    "_ZN2nn2pl21RequestSharedFontLoadENS0_14SharedFontTypeE" => super::nothing,
    "_ZN2nn2pl8IsVrModeEv" => super::zero,
    "_ZN2nn2pl11BeginVrModeEv" => begin_vr,
    "_ZN2nn2pl9EndVrModeEv" => super::nothing,
    "_ZN2nn6fontll18ScalableFontEngine8LoadFontEPcPKvjj" => load_font,
    "_ZN2nn6fontll18ScalableFontEngine10InitializeEPvj" => initialize,
    "_ZN2nn6fontll18ScalableFontEngine22GetPointerToWorkBufferEv" => work_buffer,
}
