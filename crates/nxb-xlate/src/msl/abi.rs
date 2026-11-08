//! The host ABI of the B library, mirrored from the engine's `dm/gfx/nvn/dmsh_abi.h`. The host (`nvn_refl.m`) reads the
//! binding kinds and fixed indices below. A change here must match a change of the header in the engine.

/// The binding kinds of `_b.json` (`DMSH_KIND_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// `cbuf`: uniform buffer slot `reg` (`c[3 + reg]`).
    Cbuf,
    /// `c1`: the shader's own constant segment.
    C1,
    /// `drvcb`: a driver segment `c[bank]` (bank 0, 2 or the push bank).
    DrvCb,
    /// `support`: the translator's support buffer.
    Support,
    /// `geometry`: a helper buffer of the geometry lowering, `reg` is its index.
    Geometry,
    /// `sbuf`: storage buffer slot `reg`, its address in the stage's c0 table.
    Sbuf,
    /// `sbuf_ubo`: a storage buffer whose address is in uniform slot `reg` at byte `off`.
    SbufUbo,
    /// `ubo_ptr`: compute, uniform slot `reg` read through its address in c0.
    UboPtr,
    /// `tex`: texture slot `reg`.
    Tex,
    /// `tbuf`: a buffer texture in texture slot `reg`.
    Tbuf,
    /// `img`: image slot `reg`.
    Img,
    /// `samp`: separate sampler slot `reg`.
    Samp,
    /// `samp_tex`: the sampler of the combined handle in texture slot `reg`.
    SampTex,
    /// `samp_id`: a sampler ID that is a constant in the shader.
    SampId,
    /// `tex_ubo`: bindless texture, handle in uniform slot `reg` at byte `off`.
    TexUbo,
    /// `samp_ubo`: bindless sampler.
    SampUbo,
    /// `img_ubo`: bindless image.
    ImgUbo,
    /// `tbuf_ubo`: bindless buffer texture.
    TbufUbo,
    /// `tex_c2a`: an array of c2 texture handles indexed at run time.
    TexC2a,
    /// `samp_c2a`: an array of c2 sampler handles indexed at run time.
    SampC2a,
    /// `tex_pool`: the game's texture pool.
    TexPool,
    /// `samp_pool`: the sampler pool.
    SampPool,
    /// `tex_htab`: a table of the texture handles in a uniform buffer.
    TexHtab,
    /// `samp_htab`: a table of the sampler handles in a uniform buffer.
    SampHtab,
    /// `border`: the border colour block.
    Border,
    /// `gmem_map`: the memory pool table for untraced global memory.
    GmemMap,
    /// `gmem_log`: the write log of untraced global memory.
    GmemLog,
}

impl Kind {
    /// The string in `_b.json`.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Cbuf => "cbuf",
            Kind::C1 => "c1",
            Kind::DrvCb => "drvcb",
            Kind::Support => "support",
            Kind::Geometry => "geometry",
            Kind::Sbuf => "sbuf",
            Kind::SbufUbo => "sbuf_ubo",
            Kind::UboPtr => "ubo_ptr",
            Kind::Tex => "tex",
            Kind::Tbuf => "tbuf",
            Kind::Img => "img",
            Kind::Samp => "samp",
            Kind::SampTex => "samp_tex",
            Kind::SampId => "samp_id",
            Kind::TexUbo => "tex_ubo",
            Kind::SampUbo => "samp_ubo",
            Kind::ImgUbo => "img_ubo",
            Kind::TbufUbo => "tbuf_ubo",
            Kind::TexC2a => "tex_c2a",
            Kind::SampC2a => "samp_c2a",
            Kind::TexPool => "tex_pool",
            Kind::SampPool => "samp_pool",
            Kind::TexHtab => "tex_htab",
            Kind::SampHtab => "samp_htab",
            Kind::Border => "border",
            Kind::GmemMap => "gmem_map",
            Kind::GmemLog => "gmem_log",
        }
    }
}

/// Bytes of a driver segment buffer the host fills.
pub const DRV_SIZE: u32 = 0xA00;
/// Bytes from one c2 slot's handle word to the next.
pub const C2_ROW: u32 = 8;
/// c2: combined handles, slot r at `C2_TEX + 8 r`.
pub const C2_TEX: u32 = 0x20;
/// Combined slots up to the separate texture table.
pub const C2_TEX_SLOTS: u32 = 41;
/// c2: images, slot r at `C2_IMG + 8 r`.
pub const C2_IMG: u32 = 0x120;
/// Image slots in that table.
pub const C2_IMG_SLOTS: u32 = 8;
/// c2: separate textures.
pub const C2_SEPTEX: u32 = 0x168;
/// c2: separate samplers.
pub const C2_SAMP: u32 = 0x568;
/// Slots of each separate table.
pub const C2_SEP_SLOTS: u32 = 128;
/// The texture slot of separate texture 0.
pub const SEPTEX_SLOT0: u32 = 128;
/// Bytes of one c0 table entry.
pub const C0_ROW: u32 = 0x10;
/// Storage buffer slots of a stage's c0 table.
pub const C0_SSBO_SLOTS: u32 = 16;
/// c0 storage buffer table of the vertex stage.
pub const C0_SSBO_VS: u32 = 0x110;
/// c0 storage buffer table of the compute stage.
pub const C0_SSBO_CS: u32 = 0x310;
/// c0 storage buffer table of the geometry stage.
pub const C0_SSBO_GS: u32 = 0x410;
/// c0 storage buffer table of the pixel stage.
pub const C0_SSBO_PS: u32 = 0x510;
/// c0 storage buffer table of the NVN2 task stage.
pub const C0_SSBO_TS: u32 = 0x670;
/// c0 storage buffer table of the NVN2 mesh stage.
pub const C0_SSBO_MS: u32 = 0x770;
/// A task or mesh function's buffer with the draw's first workgroup.
pub const MESH_FIRST_SLOT: u32 = 29;
/// Compute: the dispatch's workgroup counts in c0.
pub const C0_DISPATCH: u32 = 0x200;
/// NVN2 compute: the workgroup counts in c0.
pub const C0_DISPATCH_NVN2: u32 = 4;
/// Compute: uniform slot r's address and size at `C0_CS_UBO + 0x10 r`.
pub const C0_CS_UBO: u32 = 0x210;
/// Uniform slots of that table.
pub const C0_CS_UBO_SLOTS: u32 = 16;
/// The push constant bank.
pub const PUSH_BANK: u32 = 24;
/// Bytes of a push constant block.
pub const PUSH_SIZE: u32 = 0x100;
/// Graphics stages: the largest point size in c0.
pub const C0_POINT_SIZE: u32 = 0x610;
/// Pixel stage: the sample position table in c0.
pub const C0_SAMPLE_TABLE: u32 = 0x620;
/// Bytes of that table.
pub const C0_SAMPLE_BYTES: u32 = 16;
/// Pixel stage: the index words of the sample table.
pub const C0_SAMPLE_INFO: u32 = 0x630;
/// Graphics stages: the draw's base instance in c0.
pub const C0_BASE_INSTANCE: u32 = 0x644;
/// A `samp_ubo`'s `sreg` when its other word is the c2 sampler word.
pub const SREG_C2: u32 = 255;
/// The constant buffer slot of the global memory map in `res.json`.
pub const GMEM_MAP_CB_SLOT: u32 = 0xFF;
/// The constant buffer offset of the global memory write log.
pub const GMEM_LOG_CB_OFFSET: u32 = 1;
/// Bytes before element 0 of a pool buffer.
pub const POOL_HEAD: u32 = 16;
/// The fragment argument table's binding of the alpha test block.
pub const ALPHA_BUFFER: u32 = 30;
/// Sampler members one shader corrects for border colours.
pub const BORDER_SAMPLERS: u32 = 32;
/// Byte offset of sampler bit i's border factors: `BORDER_K + 16 i`.
pub const BORDER_K: u32 = 0x10;
/// The suffix of a twin sampler member's name.
pub const BORDER_TWIN: &str = "_w";
/// The support buffer: byte offset of `render_scale[0]`. Entry 0 is the render scale of the pass. Entry `1 + i` is the
/// scale of the texture or image with descriptor index `i` (`rscale` of a binding is that array index). Each entry is
/// a float, 16 bytes apart (std140).
pub const SUPPORT_RENDER_SCALE: u32 = 192;
/// The entries of `render_scale`: the pass, 64 textures and 8 images.
pub const SUPPORT_RENDER_SCALES: u32 = 73;
/// Bytes of the support buffer the shader may read. The host writes `frag_scale_count` (byte 176) as 0, so a vertex
/// and a pixel shader number their entries alike.
pub const SUPPORT_SIZE: u32 = 1392;
/// The name of the border block's member in the argument buffer.
pub const BORDER_MEMBER: &str = "dm_border";
/// The fragment argument table's binding of the logic op block.
pub const LOGIC_BUFFER: u32 = 29;
/// The function constant index of the logic op.
pub const LOGIC_FC: u32 = 200;
/// The vertex buffer index of transform feedback buffer 0. Buffers 1 to 3 follow it.
pub const TF_SLOT: u32 = 8;
/// The buffer index of the transform feedback capture state (`dm_tf_capture`).
pub const TF_STATE_SLOT: u32 = 14;
/// The buffer index of the threadgroup spill.
pub const TG_SLOT: u32 = 30;
/// Bytes of threadgroup memory Metal gives.
pub const TG_MAX: u32 = 0x8000;
/// Threadgroups of the spill buffer.
pub const TG_GROUPS: u32 = 512;
/// The name suffix of the handle array for a handle traced to no slot (`<stage>_<t|tb|s>_cb0_40000000`).
pub const DYN_HANDLE_SUFFIX: &str = "_cb0_40000000";

/// The c0 storage buffer table of a stage (`ps`, `vs`, `cs`, `gs`, `ts`, `ms`).
pub fn c0_ssbo(stage: &str) -> Option<u32> {
    Some(match stage {
        "vs" => C0_SSBO_VS,
        "cs" => C0_SSBO_CS,
        "gs" => C0_SSBO_GS,
        "ps" => C0_SSBO_PS,
        "ts" => C0_SSBO_TS,
        "ms" => C0_SSBO_MS,
        _ => return None,
    })
}
