//! Which engine runs each part of a game. The engine runs everything by default. A family or title moves one service
//! at a time to nxkit, so the nxkit runtime replaces the engine step by step.

/// The engine that runs one service.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Runtime {
    /// The the engine host at the engine pin.
    #[default]
    Engine,
    /// The nxkit runtime, linked into the host.
    Nxkit,
}

/// A part of the host that nxkit can take over. The host files are those of the engine `dm/` at `f25fa956`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Service {
    /// `nn::fs` and save sync (`host/hle_fs.c`, `host/save_sync.c`).
    Fs,
    /// Controllers, touch and vibration (`host/hle_hid.c`, `host/hle_touch.c`, `host/hle_haptics.c`).
    Input,
    /// Audio output, the audio renderer and Opus (`host/hle_audio.c`, `host/hle_audren.c`, `host/hle_codec.c`).
    Audio,
    /// Movies and video decoders (`host/hle_movie*.c`, `host/hle_codec_video.c`).
    Video,
    /// Console state, account, time, applets and web pages (`host/hle_oe.c`, `host/hle_account.c`,
    /// `host/hle_time.c`, `host/hle_misc.c`, `host/hle_swkbd.c`, `host/hle_web.c`).
    System,
    /// Network and online services that answer "unavailable" (`host/hle_net.c`, `host/hle_offline.c`).
    Offline,
    /// Frame limits in game memory (`host/hle_fpslock.c`).
    Pacing,
    /// Kernel calls and threads (`host/hle_svc.c`, `host/hle_thread.c`).
    Kernel,
    /// NVN, GL and Vulkan on Metal (`gfx/nvn`, `gfx/gl`, `gfx/vk`).
    Gpu,
}

impl Service {
    /// Every service, in a stable order.
    pub const ALL: [Service; 9] = [
        Service::Fs,
        Service::Input,
        Service::Audio,
        Service::Video,
        Service::System,
        Service::Offline,
        Service::Pacing,
        Service::Kernel,
        Service::Gpu,
    ];

    /// The stable id, as tools and the host read it (`fs`).
    pub fn id(self) -> &'static str {
        match self {
            Service::Fs => "fs",
            Service::Input => "input",
            Service::Audio => "audio",
            Service::Video => "video",
            Service::System => "system",
            Service::Offline => "offline",
            Service::Pacing => "pacing",
            Service::Kernel => "kernel",
            Service::Gpu => "gpu",
        }
    }
}
