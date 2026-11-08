//! `nn::vi` take-overs: display, layer, native window, layer crop, frame history and the HDR and VRR calls.
//! Source: nxnative `dm/gfx/hle_vi.c`.
//!
//! On a console the game opens the display with the `vi` service, creates a layer and gets the native window handle
//! of the layer. It gives the handle to `nvnWindowBuilderSetNativeWindow`. The host has no `vi` service, so the
//! display, the layer and the native window are static objects of the host. The NVN window keeps the native window
//! handle and never reads it ([`crate::nvn::window`]), so any non-null pointer is a valid handle.
//!
//! Three facts of the layer reach the present. The layer size is recorded for the presenter ([`layer_size`]). The layer
//! crop is the crop of a present when the window has none ([`present_crop`]). Each present is a frame of the layer
//! ([`frame_queued`]), which `GetLatestFrameNumber` and `ListFrameHistory` report.
//!
//! The result of a call is `nn::Result`: 0 for success and 0x2A2 for a failure.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use nxkit_abi::guest;
use nxkit_nvn::backend::Rect;

use super::{checked, log, log_once_with, table};

const RES_OK: u32 = 0;
const RES_FAIL: u32 = 0x2A2;

/// The display object. Its content is only an identity, the guest never reads it.
struct Magic(#[allow(dead_code)] u32);
/// The native window object: a magic and the layer pointer.
struct Native(#[allow(dead_code)] u32, #[allow(dead_code)] u64);

static DISPLAY: Magic = Magic(0x5053_4944);
static LAYER: Magic = Magic(0x5259_414c);
static NATIVE: Native = Native(0x4e49_574e, 0);

/// The layer size as `w | h << 16` of the last sized `CreateLayer`. 0 means none.
static LAYER_WH: AtomicU32 = AtomicU32::new(0);
/// The layer crop as `x | y << 16 | w << 32 | h << 48`. 0 means none.
static CROP: AtomicU64 = AtomicU64::new(0);

/// The size of the layer that the game made with `CreateLayer(w, h)`, or None.
///
/// The presenter of the host does not read it yet. A present shows the picture in the shape of the window texture.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn layer_size() -> Option<(u32, u32)> {
    match LAYER_WH.load(Ordering::Relaxed) {
        0 => None,
        v => Some((v & 0xffff, v >> 16)),
    }
}

fn layer_crop() -> [i32; 4] {
    let v = CROP.load(Ordering::Relaxed);
    [
        (v & 0xffff) as i32,
        (v >> 16 & 0xffff) as i32,
        (v >> 32 & 0xffff) as i32,
        (v >> 48) as i32,
    ]
}

/// The packed crop of `SetLayerCrop`. A negative value, an empty size or a value above 0xffff gives 0 (no crop).
fn pack_crop(x: i32, y: i32, w: i32, h: i32) -> u64 {
    if x < 0 || y < 0 || w <= 0 || h <= 0 || x > 0xffff || y > 0xffff || w > 0xffff || h > 0xffff {
        return 0;
    }
    x as u64 | (y as u64) << 16 | (w as u64) << 32 | (h as u64) << 48
}

fn addr<T>(object: &'static T) -> u64 {
    object as *const T as u64
}

static CROP_NOTE: AtomicBool = AtomicBool::new(false);

/// The crop that a present shows: the crop of the window, else the crop of the layer (`SetLayerCrop`). On the console
/// both become the crop of the buffer that is queued to the layer.
///
/// Link's Awakening crops only its layer. In handheld mode it draws 1280x720 into its 1920x1080 window textures and
/// crops the layer to that. A layer crop that does not fit the texture is reported once and the whole texture is shown.
/// `texture` is the size of the window texture, None when it is not known.
pub(crate) fn present_crop(window: Rect, texture: Option<(u32, u32)>) -> Rect {
    if window.width != 0 || window.height != 0 {
        return window;
    }
    let [x, y, w, h] = layer_crop();
    if w == 0 || h == 0 {
        return window;
    }
    let fits = texture.is_some_and(|(tw, th)| {
        i64::from(x) + i64::from(w) <= i64::from(tw) && i64::from(y) + i64::from(h) <= i64::from(th)
    });
    if !fits {
        log_once_with(&CROP_NOTE, || {
            let (tw, th) = texture.unwrap_or((0, 0));
            format!(
                "[RED] vi layer crop {x},{y} {w}x{h} does not fit the window texture {tw}x{th}: the present shows the whole texture"
            )
        });
        return window;
    }
    Rect { x, y, width: w, height: h }
}

/// The frames that the history keeps.
const HISTORY: usize = 8;
/// The bytes of one `nn::vi::FrameInfo`: five u64.
const FRAME_INFO: u64 = 40;

#[derive(Clone, Copy, Default)]
struct Entry {
    frame: u64,
    queued_ns: u64,
    /// The guest time at which the frame was first seen after it was queued. 0 until then.
    seen_ns: u64,
}

/// The frames that the game presented and the last `HISTORY` of them.
struct Frames {
    count: u64,
    ring: [Entry; HISTORY],
}

impl Frames {
    const fn new() -> Self {
        Self { count: 0, ring: [Entry { frame: 0, queued_ns: 0, seen_ns: 0 }; HISTORY] }
    }

    /// Counts a present at the guest time `now`. The result is its frame number, which starts at 1.
    fn queue(&mut self, now: u64) -> u64 {
        self.count += 1;
        let frame = self.count;
        self.ring[frame as usize % HISTORY] = Entry { frame, queued_ns: now, seen_ns: 0 };
        frame
    }

    /// The `FrameInfo` records of the newest frames first, at most `n`. A frame counts as shown from the first call that
    /// sees it, because the backend does not report when a frame reaches the screen. `now` is the guest time.
    fn list(&mut self, n: usize, now: u64, ticks: impl Fn(u64) -> u64) -> Vec<[u64; 5]> {
        let latest = self.count;
        let mut out = Vec::new();
        let mut f = latest;
        while f > 0 && latest - f < HISTORY as u64 && out.len() < n {
            let e = &mut self.ring[f as usize % HISTORY];
            if e.frame == f {
                if e.seen_ns == 0 {
                    e.seen_ns = now.max(1);
                }
                let shown = ticks(e.seen_ns);
                out.push([2, f, ticks(e.queued_ns), shown, shown]);
            }
            f -= 1;
        }
        out
    }
}

static FRAMES: Mutex<Frames> = Mutex::new(Frames::new());

fn frames() -> std::sync::MutexGuard<'static, Frames> {
    FRAMES.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The guest clock in nanoseconds: the clock of the NVN context, 0 before there is one.
fn guest_ns() -> u64 {
    crate::nvn::ctx::settings_if_installed().map_or(0, |s| (s.clock_ns)())
}

/// The system ticks of the Switch (19.2 MHz) for a time in nanoseconds.
fn ticks(ns: u64) -> u64 {
    (u128::from(ns) * 12 / 625) as u64
}

/// The game presents a frame: it is the next frame of the layer. Called by `nvnQueuePresentTexture`.
pub(crate) fn frame_queued() {
    let now = guest_ns();
    frames().queue(now);
}

/// `nn::vi::Initialize()`: the host has no vi service.
extern "C" fn initialize() {}

/// `nn::vi::OpenDefaultDisplay(Display **out)`: the host returns a static object.
extern "C" fn open_default_display(out: u64) -> u32 {
    checked("nn::vi::OpenDefaultDisplay", guest::write_u64(out, addr(&DISPLAY)));
    RES_OK
}

/// `nn::vi::OpenDisplay(Display **out, const char *name)`: only the display `Default` exists. A display that
/// `ListDisplays` returned can also be opened by name. The SDK `OpenDisplay` still reaches the absent vi service
/// even though `Initialize` is taken over (Alien: Isolation, SDK 8.2.0: a null read), so it shares the default display
/// object with `OpenDefaultDisplay`.
extern "C" fn open_display(out: u64, name: u64) -> u32 {
    if name == 0 {
        return RES_FAIL;
    }
    let name = checked("nn::vi::OpenDisplay", guest::read_cstr(name, 64));
    if name != b"Default" {
        return RES_FAIL;
    }
    open_default_display(out)
}

/// `nn::vi::CreateLayer(Layer **out, Display *disp)`: the host returns a static object.
extern "C" fn create_layer(out: u64, _disp: u64) -> u32 {
    checked("nn::vi::CreateLayer", guest::write_u64(out, addr(&LAYER)));
    RES_OK
}

/// `nn::vi::CreateLayer(Layer **out, Display *disp, int w, int h)`: the overload with the size of the layer (the Switch
/// video driver of SDL3, Absolum: 1920x1080 docked). The SDK version calls through the vi service object that the host
/// never made. A window of NVN or Vulkan has its textures decide what is presented. The size is recorded for the
/// presenter ([`layer_size`]).
extern "C" fn create_layer_wh(out: u64, _disp: u64, w: u32, h: u32) -> u32 {
    let (w, h) = (w as i32, h as i32);
    log(format_args!("vi::CreateLayer {w}x{h}"));
    checked("nn::vi::CreateLayer", guest::write_u64(out, addr(&LAYER)));
    if w > 0 && h > 0 && w < 0x10000 && h < 0x10000 {
        LAYER_WH.store(w as u32 | (h as u32) << 16, Ordering::Relaxed);
    }
    RES_OK
}

/// `nn::vi::CreateLayer(Layer **out, Display *disp, const LayerCreationSettings *settings)`: the overload of SDK 22. Its
/// constructor `LayerCreationSettings(int w, int h)` fills the settings as {w, h, 2} (EA SPORTS FC 27). The SDK
/// `CreateLayer` goes to `am::GetSelfController()`, which the host never made.
extern "C" fn create_layer_settings(out: u64, disp: u64, settings: u64) -> u32 {
    let (w, h) = if settings == 0 {
        (0, 0)
    } else {
        let name = "nn::vi::CreateLayer";
        (
            checked(name, guest::read_u32(settings)),
            checked(name, guest::read_u32(settings + 4)),
        )
    };
    create_layer_wh(out, disp, w, h)
}

/// `nn::vi::GetNativeWindow(void **out, Layer *layer)`: the handle that goes to the NVN window builder. The host
/// returns a static object.
extern "C" fn get_native_window(out: u64, _layer: u64) -> u32 {
    checked("nn::vi::GetNativeWindow", guest::write_u64(out, addr(&NATIVE)));
    RES_OK
}

/// `nn::vi::SetLayerCrop(Layer *layer, int x, int y, int w, int h)`: the host records the crop and the present crops
/// by it. Link's Awakening also crops its layer and tears the display down on exit. The SDK version dereferences the
/// vi service objects that the host never made. A present with a window crop of its own does not read it.
extern "C" fn set_layer_crop(_layer: u64, x: u32, y: u32, w: u32, h: u32) -> u32 {
    let (x, y, w, h) = (x as i32, y as i32, w as i32, h as i32);
    let packed = pack_crop(x, y, w, h);
    if CROP.swap(packed, Ordering::Relaxed) != packed {
        log(format_args!("vi::SetLayerCrop {x},{y} {w}x{h}"));
    }
    RES_OK
}

/// `nn::vi::SetLayerScalingMode(Layer *layer, ScalingMode mode)`: the present of the host scales the picture itself.
/// Super Mario Odyssey sets the mode and the SDK version uses the vi service objects that the host never made.
extern "C" fn set_layer_scaling_mode(_layer: u64, mode: u32) -> u32 {
    log(format_args!("vi::SetLayerScalingMode {} (ignored)", mode as i32));
    RES_OK
}

/// `nn::vi::GetLatestFrameNumber(uint64_t *out, const Layer *layer)`: the frames that the game has presented so far.
/// Unreal Engine paces its frames by it (Octopath Traveler 0). The SDK versions read the Android surface of the vi
/// service that the host never made.
extern "C" fn get_latest_frame_number(out: u64, _layer: u64) -> u32 {
    let count = frames().count;
    checked("nn::vi::GetLatestFrameNumber", guest::write_u64(out, count));
    RES_OK
}

static HISTORY_NOTE: AtomicBool = AtomicBool::new(false);

/// `nn::vi::ListFrameHistory(FrameInfo *out, int n, const Layer *layer)`: the last 8 presents, newest first. Unreal
/// Engine 5 tracks its flips by it. Its "Viewport Thread" asks how many entries there are (a null buffer: the answer
/// is 8, the size of the history), reads them and takes the first one shown as the latest flip. That completes the
/// fences at the end of the frame. Without a history no flip ever comes and the first `FlushRenderingCommands` waits
/// for ever (Minecraft Dungeons II).
///
/// `FrameInfo` is 40 bytes: the status (2 shown with a present time, 1 queued and not shown yet), the frame number
/// (the numbering of `GetLatestFrameNumber`), the queue tick and the present tick twice. The backend does not report
/// when a frame reaches the screen. A frame is shown from the first call that sees it, with that time as its present
/// time. The ticks are 19.2 MHz. An SDK of the Switch 2 counts at another rate, which the host cannot tell apart.
extern "C" fn list_frame_history(out: u64, n: u32, _layer: u64) -> u32 {
    const NAME: &str = "nn::vi::ListFrameHistory";
    let n = n as i32;
    if out == 0 || n <= 0 {
        return HISTORY as u32;
    }
    let now = guest_ns();
    let (records, latest) = {
        let mut f = frames();
        (f.list(n as usize, now, ticks), f.count)
    };
    for (i, r) in records.iter().enumerate() {
        for (j, v) in r.iter().enumerate() {
            checked(NAME, guest::write_u64(out + i as u64 * FRAME_INFO + j as u64 * 8, *v));
        }
    }
    if !records.is_empty() {
        log_once_with(&HISTORY_NOTE, || {
            format!(
                "vi::ListFrameHistory: {} of the last {HISTORY} presents returned (up to frame {latest})",
                records.len()
            )
        });
    }
    records.len() as u32
}

/// `nn::vi::GetDefaultDisplayPanelIdentifier(PanelIdentifier *out)`: the host has no panel, so the identifier is all
/// zero. Rhythm Heaven Groove (SDK 21) asks for it at start. The game reads a u32 kind and, when it is 2 or more,
/// 0x14 more bytes that it hands on. The SDK version calls through the vi service object that the host never made.
extern "C" fn get_default_display_panel_identifier(out: u64) -> u32 {
    checked("nn::vi::GetDefaultDisplayPanelIdentifier", guest::fill(out, 0x18, 0));
    RES_OK
}

/// `nn::vi::ListDisplays(DisplayInfo *out, int n)`: the host has one display, `Default`. The Great Ace Attorney
/// Chronicles (SDK 10) lists up to 10 displays before it opens the default one and does not read the list.
/// `DisplayInfo` is 0x60 bytes: the name in 0x40 bytes, a flag that a layer limit exists at 0x40, then three u64: the
/// layer count limit, the largest layer width and height.
extern "C" fn list_displays(out: u64, n: u32) -> u32 {
    const NAME: &str = "nn::vi::ListDisplays";
    if n as i32 <= 0 || out == 0 {
        return 0;
    }
    checked(NAME, guest::fill(out, 0x60, 0));
    checked(NAME, guest::write(out, b"Default"));
    checked(NAME, guest::write_u8(out + 0x40, 1));
    for (i, v) in [1u64, 1920, 1080].into_iter().enumerate() {
        checked(NAME, guest::write_u64(out + 0x48 + 8 * i as u64, v));
    }
    1
}

/// `nn::vi::DestroyLayer(Layer *layer)`: the layer is a static object of the host.
extern "C" fn destroy_layer(_layer: u64) {}

/// `nn::vi::CloseDisplay(Display *disp)`: the display is a static object of the host.
extern "C" fn close_display(_disp: u64) {}

/// `nn::vi::Finalize()`: the host has no vi service.
extern "C" fn finalize() {}

/// `nn::vi::QueryDisplayHdrStatus(bool *on, HdrOutputFormat *fmt, uint16_t *peak, const Display *disp)`: SDR output,
/// because the host presents SDR. `HdrOutputFormat` 0 is SDR. The SDK versions check the display against their own
/// default display object and forward the layer to vi2, whose layer object the host never made (Donkey Kong
/// Bananza: a call through null from `SetLayerNextHdrContentGamutMetadata`).
extern "C" fn query_display_hdr_status(on: u64, fmt: u64, peak: u64, _disp: u64) -> u32 {
    const NAME: &str = "nn::vi::QueryDisplayHdrStatus";
    if on != 0 {
        checked(NAME, guest::write_u8(on, 0));
    }
    if fmt != 0 {
        checked(NAME, guest::write_u32(fmt, 0));
    }
    if peak != 0 {
        checked(NAME, guest::write_u16(peak, 0));
    }
    RES_OK
}

/// `nn::vi::QueryDisplayHdrPaperWhite(uint16_t *nits, const Display *disp)`: 203 nits, the reference of BT.2408. The
/// game reads it only when HDR is on.
extern "C" fn query_display_hdr_paper_white(nits: u64, _disp: u64) -> u32 {
    if nits != 0 {
        checked("nn::vi::QueryDisplayHdrPaperWhite", guest::write_u16(nits, 203));
    }
    RES_OK
}

/// `nn::vi::QueryDisplayVrrSupported(bool *on, const Display *disp)`: not supported, the refresh rate is fixed.
extern "C" fn query_display_vrr_supported(on: u64, _disp: u64) -> u32 {
    if on != 0 {
        checked("nn::vi::QueryDisplayVrrSupported", guest::write_u8(on, 0));
    }
    RES_OK
}

/// `nn::vi::SetDisplayVrrCapability(Display *disp, bool on)`: the host has no VRR.
extern "C" fn set_display_vrr_capability(_disp: u64, _on: u32) -> u32 {
    RES_OK
}

static MAX_RES_NOTE: AtomicBool = AtomicBool::new(false);

/// `nn::vi::QueryDisplayBufferMaxResolution(BufferMaxResolution *out, const Display *disp)`: the largest display
/// buffer. `BufferMaxResolution` is 0 for 1280x720, 1 for 1920x1080, 2 for 2560x1440 and 3 for 3840x2160 (the table of
/// Fire Emblem Fortune's Weave). The SDK reaches vi2, which the host does not serve. The call fails with 0x272 and the
/// output stays as the game set it. Fire Emblem Fortune's Weave sets 0 first, so the failed call told it 720p. It then
/// chose its 1280x720 network and rendered at 640x360.
///
/// The answer is 1, 1920x1080, as the screen of a Switch 2 in handheld mode. nxnative answers the largest of the four
/// that the display holds when the console is docked. nxkit has no fact of the size of the display, so a docked console
/// answers 1 too.
extern "C" fn query_display_buffer_max_resolution(out: u64, _disp: u64) -> u32 {
    log_once_with(&MAX_RES_NOTE, || {
        "vi::QueryDisplayBufferMaxResolution: 1920x1080 (the size of the display is not known)".to_string()
    });
    if out != 0 {
        checked(
            "nn::vi::QueryDisplayBufferMaxResolution",
            guest::write_u32(out, 1),
        );
    }
    RES_OK
}

static SWAP_NOTE: AtomicBool = AtomicBool::new(false);

/// `nn::vi::SetLayerExtendedSwapInterval(Layer *layer, int a, int b)`: asks for a rate between whole vsync intervals
/// (VRR). The host has no VRR, so the swap interval of NVN stays.
extern "C" fn set_layer_extended_swap_interval(_layer: u64, a: u32, b: u32) -> u32 {
    log_once_with(&SWAP_NOTE, || {
        format!(
            "vi::SetLayerExtendedSwapInterval {} {}: no VRR on the host, the NVN swap interval stays",
            a as i32, b as i32
        )
    });
    RES_OK
}

/// `nn::vi::ClearLayerExtendedSwapInterval(Layer *layer)`.
extern "C" fn clear_layer_extended_swap_interval(_layer: u64) -> u32 {
    RES_OK
}

/// `nn::vi::SetLayerNextHdrContentGamutMetadata(Layer *layer, HdrContentGamut g)`: SDR output, nothing to apply.
extern "C" fn set_layer_next_hdr_content_gamut(_layer: u64, _gamut: u32) -> u32 {
    RES_OK
}

/// `nn::vi::SetLayerNextHdrToSdrToneMappingMetadata(Layer *layer, HdrToneMapping m)`: SDR output, nothing to apply.
extern "C" fn set_layer_next_hdr_to_sdr_tone_mapping(_layer: u64, _mapping: u32) -> u32 {
    RES_OK
}

/// `nn::vi::SetLayerNextHdrExtendedContentLuminanceRangeMetadata(Layer *layer, uint16_t lo, uint16_t hi)`: SDR output,
/// nothing to apply.
extern "C" fn set_layer_next_hdr_extended_luminance(_layer: u64, _lo: u32, _hi: u32) -> u32 {
    RES_OK
}

/// `nn::vi::SetLayerNextHdrStandardContentLuminanceRangeMetadata(Layer *layer, uint16_t lo, uint16_t hi)`: SDR
/// output, nothing to apply.
extern "C" fn set_layer_next_hdr_standard_luminance(_layer: u64, _lo: u32, _hi: u32) -> u32 {
    RES_OK
}

/// `nn::vi::SetLayerNextHdrAdjustableBezierMetadata(Layer *layer, float a, float b, float c)`: SDR output, nothing to
/// apply. The rest of the HDR metadata calls of SDK 22 (the Bezier tone curve and the clears): the SDK forwards them
/// through a table that its graphics system fills, which is null on the host. Unanswered they call through null (Mario
/// Kart World 1.8.0 sets the curve every frame).
extern "C" fn set_layer_next_hdr_bezier(_layer: u64, _a: f32, _b: f32, _c: f32) -> u32 {
    RES_OK
}

/// `nn::vi::ClearLayerNextHdrAdjustableBezierMetadata(Layer *layer)`: SDR output, nothing to clear.
extern "C" fn clear_layer_next_hdr_bezier(_layer: u64) -> u32 {
    RES_OK
}

/// `nn::vi::ClearLayerNextHdrContentGamutMetadata(Layer *layer)`: SDR output, nothing to clear.
extern "C" fn clear_layer_next_hdr_content_gamut(_layer: u64) -> u32 {
    RES_OK
}

/// `nn::vi::ClearLayerNextHdrToSdrToneMappingMetadata(Layer *layer)`: SDR output, nothing to clear.
extern "C" fn clear_layer_next_hdr_to_sdr_tone_mapping(_layer: u64) -> u32 {
    RES_OK
}

table! {
    "_ZN2nn2vi10InitializeEv" => initialize,
    "_ZN2nn2vi18OpenDefaultDisplayEPPNS0_7DisplayE" => open_default_display,
    "_ZN2nn2vi11OpenDisplayEPPNS0_7DisplayEPKc" => open_display,
    "_ZN2nn2vi11CreateLayerEPPNS0_5LayerEPNS0_7DisplayE" => create_layer,
    "_ZN2nn2vi11CreateLayerEPPNS0_5LayerEPNS0_7DisplayEii" => create_layer_wh,
    "_ZN2nn2vi11CreateLayerEPPNS0_5LayerEPNS0_7DisplayEPKNS0_21LayerCreationSettingsE" => create_layer_settings,
    "_ZN2nn2vi15GetNativeWindowEPPvPNS0_5LayerE" => get_native_window,
    "_ZN2nn2vi12SetLayerCropEPNS0_5LayerEiiii" => set_layer_crop,
    "_ZN2nn2vi19SetLayerScalingModeEPNS0_5LayerENS0_11ScalingModeE" => set_layer_scaling_mode,
    "_ZN2nn2vi20GetLatestFrameNumberEPmPKNS0_5LayerE" => get_latest_frame_number,
    "_ZN2nn2vi16ListFrameHistoryEPNS0_9FrameInfoEiPKNS0_5LayerE" => list_frame_history,
    "_ZN2nn2vi32GetDefaultDisplayPanelIdentifierEPNS0_15PanelIdentifierE" => get_default_display_panel_identifier,
    "_ZN2nn2vi12ListDisplaysEPNS0_11DisplayInfoEi" => list_displays,
    "_ZN2nn2vi12DestroyLayerEPNS0_5LayerE" => destroy_layer,
    "_ZN2nn2vi12CloseDisplayEPNS0_7DisplayE" => close_display,
    "_ZN2nn2vi8FinalizeEv" => finalize,
    "_ZN2nn2vi21QueryDisplayHdrStatusEPbPNS0_15HdrOutputFormatEPtPKNS0_7DisplayE" => query_display_hdr_status,
    "_ZN2nn2vi25QueryDisplayHdrPaperWhiteEPtPKNS0_7DisplayE" => query_display_hdr_paper_white,
    "_ZN2nn2vi24QueryDisplayVrrSupportedEPbPKNS0_7DisplayE" => query_display_vrr_supported,
    "_ZN2nn2vi23SetDisplayVrrCapabilityEPNS0_7DisplayEb" => set_display_vrr_capability,
    "_ZN2nn2vi31QueryDisplayBufferMaxResolutionEPNS0_19BufferMaxResolutionEPKNS0_7DisplayE" => query_display_buffer_max_resolution,
    "_ZN2nn2vi28SetLayerExtendedSwapIntervalEPNS0_5LayerEii" => set_layer_extended_swap_interval,
    "_ZN2nn2vi30ClearLayerExtendedSwapIntervalEPNS0_5LayerE" => clear_layer_extended_swap_interval,
    "_ZN2nn2vi35SetLayerNextHdrContentGamutMetadataEPNS0_5LayerENS0_15HdrContentGamutE" => set_layer_next_hdr_content_gamut,
    "_ZN2nn2vi39SetLayerNextHdrToSdrToneMappingMetadataEPNS0_5LayerENS0_14HdrToneMappingE" => set_layer_next_hdr_to_sdr_tone_mapping,
    "_ZN2nn2vi52SetLayerNextHdrExtendedContentLuminanceRangeMetadataEPNS0_5LayerEtt" => set_layer_next_hdr_extended_luminance,
    "_ZN2nn2vi52SetLayerNextHdrStandardContentLuminanceRangeMetadataEPNS0_5LayerEtt" => set_layer_next_hdr_standard_luminance,
    "_ZN2nn2vi39SetLayerNextHdrAdjustableBezierMetadataEPNS0_5LayerEfff" => set_layer_next_hdr_bezier,
    "_ZN2nn2vi41ClearLayerNextHdrAdjustableBezierMetadataEPNS0_5LayerE" => clear_layer_next_hdr_bezier,
    "_ZN2nn2vi37ClearLayerNextHdrContentGamutMetadataEPNS0_5LayerE" => clear_layer_next_hdr_content_gamut,
    "_ZN2nn2vi41ClearLayerNextHdrToSdrToneMappingMetadataEPNS0_5LayerE" => clear_layer_next_hdr_to_sdr_tone_mapping,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvn::abi::testing;

    fn rect(x: i32, y: i32, width: i32, height: i32) -> Rect {
        Rect { x, y, width, height }
    }

    /// The layer state is global, so one test holds it.
    #[test]
    fn the_layer_size_and_crop_follow_the_calls() {
        let out = testing::alloc(0x100);
        assert_eq!(create_layer_wh(out, 0, 1920, 1080), RES_OK);
        assert_eq!(guest::read_u64(out).expect("layer"), addr(&LAYER));
        assert_eq!(layer_size(), Some((1920, 1080)));
        assert_eq!(create_layer_wh(out, 0, 0, 1080), RES_OK);
        assert_eq!(layer_size(), Some((1920, 1080)));

        let settings = testing::alloc(16);
        guest::write_u32(settings, 1280).expect("w");
        guest::write_u32(settings + 4, 720).expect("h");
        create_layer_settings(out, 0, settings);
        assert_eq!(layer_size(), Some((1280, 720)));

        set_layer_crop(0, 0, 0, 1280, 720);
        assert_eq!(present_crop(rect(0, 0, 0, 0), Some((1920, 1080))), rect(0, 0, 1280, 720));
        assert_eq!(
            present_crop(rect(1, 2, 3, 4), Some((1920, 1080))),
            rect(1, 2, 3, 4),
            "the crop of the window wins"
        );
        assert_eq!(
            present_crop(rect(0, 0, 0, 0), Some((1000, 1080))),
            rect(0, 0, 0, 0),
            "a crop that does not fit shows the whole texture"
        );
        set_layer_crop(0, u32::MAX, 0, 10, 10);
        assert_eq!(present_crop(rect(0, 0, 0, 0), Some((1920, 1080))), rect(0, 0, 0, 0));
    }

    #[test]
    fn the_crop_packs_only_valid_values() {
        assert_eq!(pack_crop(0, 0, 0, 5), 0);
        assert_eq!(pack_crop(-1, 0, 5, 5), 0);
        assert_eq!(pack_crop(0, 0x1_0000, 5, 5), 0);
        assert_eq!(pack_crop(1, 2, 3, 4), 1 | 2 << 16 | 3 << 32 | 4 << 48);
    }

    #[test]
    fn the_frame_history_lists_the_newest_frames_first() {
        let mut f = Frames::new();
        for i in 1..=10 {
            assert_eq!(f.queue(i * 1000), i);
        }
        let all = f.list(20, 50_000, |ns| ns);
        assert_eq!(all.len(), HISTORY);
        assert_eq!(all[0], [2, 10, 10_000, 50_000, 50_000]);
        assert_eq!(all[7][1], 3);
        assert_eq!(f.list(2, 60_000, |ns| ns).len(), 2);
        assert_eq!(f.list(1, 60_000, |ns| ns)[0][3], 50_000, "the time of the first sight stays");
        assert!(Frames::new().list(8, 1, |ns| ns).is_empty());
    }

    #[test]
    fn the_ticks_run_at_19_2_mhz() {
        assert_eq!(ticks(1_000_000_000), 19_200_000);
    }

    #[test]
    fn the_display_list_and_the_names() {
        let out = testing::alloc(0x100);
        assert_eq!(list_displays(0, 10), 0);
        assert_eq!(list_displays(out, 0), 0);
        assert_eq!(list_displays(out, 10), 1);
        assert_eq!(guest::read_u8(out).expect("name"), b'D');
        assert_eq!(guest::read_u8(out + 0x40).expect("flag"), 1);
        assert_eq!(guest::read_u64(out + 0x50).expect("width"), 1920);

        let name = testing::alloc(16);
        guest::write(name, b"Default\0").expect("name");
        assert_eq!(open_display(out, name), RES_OK);
        assert_eq!(guest::read_u64(out).expect("display"), addr(&DISPLAY));
        guest::write(name, b"Other\0").expect("name");
        assert_eq!(open_display(out, name), RES_FAIL);
        assert_eq!(open_display(out, 0), RES_FAIL);
        assert_eq!(list_frame_history(0, 4, 0), HISTORY as u32);
    }

    #[test]
    fn the_exports_are_the_names_of_the_list() {
        let names: Vec<&str> = exports().iter().map(|e| e.name).collect();
        assert_eq!(names.len(), 31);
        assert!(names.iter().all(|n| n.starts_with("_ZN2nn2vi")));
    }
}
