//! The run settings of a game: what the engine keeps in `dm/games/<id>/game.tsv` and generates into
//! `dm/host/gen/<id>/game_profile.h` (`game.py` `header_one`). `nxb build` writes them into `game.json`.
//! The defaults are those of `game.py`.

/// The guest memory layout and the sizes `svc::GetInfo` reports (`dm/host/loader.c`, `main.c`, `hle_svc.c`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Memory {
    /// `mem_reserve`: the address space the host reserves for the game, in bytes.
    pub reserve: u64,
    /// `mem_alias`: the size of the alias region.
    pub alias: u64,
    /// `mem_stack`: the size of the stack region.
    pub stack: u64,
    /// `app_pool`: the application memory pool (`GetInfo` total memory size).
    pub app_pool: u64,
    /// `mem_used`: the used memory reported beside the heap (`GetInfo` UsedMemorySize).
    pub used: u64,
    /// `sys_resource`: the system resource size reported (`GetInfo` TotalSystemResourceSize). 0 means no virtual
    /// address memory.
    pub system_resource: u64,
    /// `mem_map_limit`: `MapPhysicalMemory` is refused beyond the application pool.
    pub map_limit: bool,
    /// `stdalloc_cap`: the address space of `nn::mem::StandardAllocator` with virtual address memory. 0 means what the
    /// alias region has room for.
    pub stdalloc_cap: u64,
    /// `alloc_big_align`: `StandardAllocator::Allocate` aligns blocks of 64 KB or more to 64 KB.
    pub alloc_big_align: bool,
}

/// The other settings of a game's run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The memory layout.
    pub memory: Memory,
    /// `game_args`: the game's arguments when the command line gives none.
    pub args: &'static str,
    /// `game_env`: `KEY=VALUE` pairs that `getenv` answers.
    pub env: &'static str,
    /// `launch_param`: what `oe::TryPopLaunchParameter` hands over once.
    pub launch_param: &'static str,
    /// The builds the game shows a version for: (build ID of `main` as 32 hex digits, version text).
    pub display_versions: &'static [(&'static str, &'static str)],
    /// `core_preempt`: the same-core preemption approximation.
    pub core_preempt: bool,
    /// `present_interval`: `true` is one frame per display tick, `false` is as the game sets it.
    pub present_display: bool,
    /// `fps_target`: 0 (the game's own step), 30 or 60.
    pub fps_target: u32,
    /// `fps_max`: 60 or 120.
    pub fps_max: u32,
    /// `fps_hold`: the frame step may hold a frame longer when frames come late.
    pub fps_hold: bool,
    /// `texture_pool_mb`: the size of the engine's texture streaming pool in MB, 0 for the game's own.
    /// A game of an engine with such a pool states it. The core knows no engine.
    pub texture_pool_mb: u32,
    /// `fps_cvar`: the name of the engine variable the host writes `fps_target` into, `None` when the game has none.
    pub fps_cvar: Option<&'static str>,
    /// `present_snap`: present snapshots by default.
    pub present_snap: bool,
    /// `pso_mtl4`: pipelines through `MTL4Compiler` with the archive.
    pub pso_mtl4: bool,
    /// `enc_thread`: Metal encoding on its own thread.
    pub enc_thread: bool,
    /// `overlap`: the CPU runs one frame ahead of the GPU; `nvnSyncWait` for the newest frame waits for the one
    /// before it (`docs/plans/fence-overlap.md`).
    pub overlap: bool,
    /// `screen_aspect`: stubs that follow a screen taller than 16:9.
    pub screen_aspect: bool,
    /// `shared_font`: `bfttf` (8-byte header and TTF) or `ttf`. `None`: the host decides by the SDK version.
    pub shared_font: Option<&'static str>,
    /// `pad_layout`: `simple`, `full` or `touch`.
    pub pad_layout: &'static str,
    /// `op_mode`: `console` (docked) or `handheld`.
    pub op_mode: &'static str,
    /// `perf_mode`: `true` reports Boost always, `false` follows the operation mode.
    pub perf_boost: bool,
    /// `tex_flush_upload`.
    pub tex_flush_upload: bool,
    /// `tex_dirty_upload`.
    pub tex_dirty_upload: bool,
    /// `tex_dyn_slots`.
    pub tex_dyn_slots: bool,
    /// `render_cond`.
    pub render_cond: bool,
    /// `vconv_range`.
    pub vconv_range: bool,
    /// `gpu_timestamps`: `true` is `segment`, `false` is `submission`.
    pub gpu_ts_segment: bool,
    /// `draw_barriers`: `true` is `game`, `false` is `full`.
    pub draw_barriers_game: bool,
    /// `shader_key_skip`: (offset, length) of the control bytes the shader keys hash as zeros, (0, 0) for none.
    pub shader_key_skip: (u32, u32),
    /// The shader keys of the game's own upscaler.
    pub upscaler_keys: &'static [&'static str],
    /// `device_api`: the NVN API (major, minor) that `nvnDeviceGetInteger` reports. `None`: by the SDK version.
    pub nvn_api: Option<(u32, u32)>,
}

impl Settings {
    /// What `game.py` takes for a key a `game.tsv` does not set. A title sets its own values over it.
    pub const DEFAULT: Settings = Settings {
        memory: Memory {
            reserve: 0,
            alias: 0,
            stack: 0,
            app_pool: 0,
            used: 0x400_0000,
            system_resource: 0x200_0000,
            map_limit: false,
            stdalloc_cap: 0,
            alloc_big_align: false,
        },
        args: "",
        env: "",
        launch_param: "",
        display_versions: &[],
        core_preempt: false,
        present_display: false,
        fps_target: 0,
        fps_max: 60,
        fps_hold: true,
        texture_pool_mb: 0,
        fps_cvar: None,
        present_snap: false,
        pso_mtl4: false,
        enc_thread: false,
        overlap: false,
        screen_aspect: false,
        shared_font: None,
        pad_layout: "simple",
        op_mode: "console",
        perf_boost: false,
        tex_flush_upload: false,
        tex_dirty_upload: false,
        tex_dyn_slots: false,
        render_cond: false,
        vconv_range: false,
        gpu_ts_segment: false,
        draw_barriers_game: false,
        shader_key_skip: (0, 0),
        upscaler_keys: &[],
        nvn_api: None,
    };
}
