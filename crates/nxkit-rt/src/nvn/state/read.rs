//! Readers of the state objects for the command buffer group. The values of the enums are the raw NVN values.

use super::{
    AbiResult, BL_ADV, BL_ADV_DST_FREE, BL_ADV_NPM, BL_ADV_OVL, BL_DST_A, BL_DST_RGB, BL_EQ_A,
    BL_EQ_RGB, BL_SRC_A, BL_SRC_RGB, BL_TARGET, BLEND, COLOR, COLOR_ALPHA_FUNC, COLOR_BLEND_EN,
    COLOR_LOGIC_OP, DS, DS_DF, DS_DT, DS_DW, DS_FAIL_B, DS_FAIL_F, DS_SF_B, DS_SF_F, DS_ST,
    DS_ZFAIL_B, DS_ZFAIL_F, DS_ZPASS_B, DS_ZPASS_F, MS_A2C, MS_A2C_DITHER, MS_C2C_EN, MS_C2C_OUT,
    MS_COV_MOD, MS_ENABLE, MS_RASTER, MS_SAMPLES, MS_SL_EN, MS_SL_GRID, Obj, PO_CULL, PO_FRONT,
    PO_MODE, PO_OFFSET, POLY, VA, VA_FMT, VA_OFFSET, VA_STREAM, VS_DIVISOR, VS_STRIDE, byte, load,
    unpack,
};

/// A blend state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlendState {
    /// The render target the state applies to.
    pub(crate) target: u32,
    /// `NVNblendFunc` of the source color.
    pub(crate) src_rgb: u32,
    /// `NVNblendFunc` of the destination color.
    pub(crate) dst_rgb: u32,
    /// `NVNblendFunc` of the source alpha.
    pub(crate) src_alpha: u32,
    /// `NVNblendFunc` of the destination alpha.
    pub(crate) dst_alpha: u32,
    /// `NVNblendEquation` of the color.
    pub(crate) eq_rgb: u32,
    /// `NVNblendEquation` of the alpha.
    pub(crate) eq_alpha: u32,
    /// `NVNblendAdvancedMode`, 0 for none.
    pub(crate) advanced_mode: u32,
    /// The source is premultiplied.
    pub(crate) premultiplied_src: bool,
    /// The destination is normalized.
    pub(crate) normalized_dst: bool,
    /// `NVNblendAdvancedOverlap`.
    pub(crate) overlap: u32,
}

/// A color state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ColorState {
    /// Bit n is set when render target n blends.
    pub(crate) blend_enable: u8,
    /// `NVNlogicOp`.
    pub(crate) logic_op: u32,
    /// `NVNalphaFunc`.
    pub(crate) alpha_func: u32,
}

impl ColorState {
    /// True when render target `i` blends.
    pub(crate) fn blends(&self, i: u32) -> bool {
        i < 8 && self.blend_enable >> i & 1 != 0
    }

    /// True when any render target blends.
    pub(crate) fn any_blend(&self) -> bool {
        self.blend_enable != 0
    }
}

/// A channel mask state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChannelMaskState {
    /// Four bits per target. Red is 1, green 2, blue 4 and alpha 8.
    pub(crate) mask: u32,
}

impl ChannelMaskState {
    /// The channels of render target `i`, 0 for an index above 7.
    pub(crate) fn channels(&self, i: u32) -> u8 {
        if i < 8 {
            (self.mask >> (4 * i) & 0xf) as u8
        } else {
            0
        }
    }

    /// Bit n is set when render target n has at least one channel to write.
    pub(crate) fn written_targets(&self) -> u8 {
        (0..8).fold(0, |bits, i| bits | u8::from(self.channels(i) != 0) << i)
    }
}

/// The stencil settings of one face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StencilFace {
    /// `NVNstencilFunc`.
    pub(crate) func: u32,
    /// `NVNstencilOp` when the stencil test fails.
    pub(crate) fail: u32,
    /// `NVNstencilOp` when the depth test fails.
    pub(crate) zfail: u32,
    /// `NVNstencilOp` when both tests pass.
    pub(crate) zpass: u32,
}

/// A depth stencil state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DepthStencilState {
    /// The depth test is on.
    pub(crate) depth_test: bool,
    /// The draw writes depth.
    pub(crate) depth_write: bool,
    /// `NVNdepthFunc`.
    pub(crate) depth_func: u32,
    /// The stencil test is on.
    pub(crate) stencil_test: bool,
    /// The front face.
    pub(crate) front: StencilFace,
    /// The back face.
    pub(crate) back: StencilFace,
}

/// A multisample state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MultisampleState {
    /// Multisampling is on.
    pub(crate) enable: bool,
    /// The number of samples.
    pub(crate) samples: u8,
    /// Alpha to coverage is on.
    pub(crate) alpha_to_coverage: bool,
    /// Alpha to coverage dithers.
    pub(crate) alpha_to_coverage_dither: bool,
    /// The number of raster samples.
    pub(crate) raster_samples: u8,
    /// `NVNcoverageModulationMode`.
    pub(crate) coverage_modulation: u8,
    /// Coverage to color is on.
    pub(crate) coverage_to_color: bool,
    /// The output of coverage to color.
    pub(crate) coverage_to_color_output: u8,
    /// Programmable sample locations are on.
    pub(crate) sample_locations: bool,
    /// The sample locations form a grid.
    pub(crate) sample_locations_grid: bool,
}

/// A polygon state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PolygonState {
    /// `NVNface` that is culled.
    pub(crate) cull_face: u32,
    /// `NVNfrontFace`.
    pub(crate) front_face: u32,
    /// `NVNpolygonMode`.
    pub(crate) mode: u32,
    /// `NVNpolygonOffsetEnable` bits.
    pub(crate) offset_enables: u32,
}

/// A vertex attribute state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VertexAttribState {
    /// `NVNformat`.
    pub(crate) format: u32,
    /// The byte offset in the vertex.
    pub(crate) offset: u32,
    /// The vertex stream the attribute reads.
    pub(crate) stream: u32,
}

/// A vertex stream state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VertexStreamState {
    /// The stride in bytes.
    pub(crate) stride: u32,
    /// The instance divisor.
    pub(crate) divisor: u32,
}

/// Reads the `NVNblendState` at the guest address `s`.
pub(crate) fn blend(s: u64) -> AbiResult<BlendState> {
    let w = load(s, BLEND)?;
    Ok(BlendState {
        target: unpack(w, BL_TARGET),
        src_rgb: unpack(w, BL_SRC_RGB),
        dst_rgb: unpack(w, BL_DST_RGB),
        src_alpha: unpack(w, BL_SRC_A),
        dst_alpha: unpack(w, BL_DST_A),
        eq_rgb: unpack(w, BL_EQ_RGB),
        eq_alpha: unpack(w, BL_EQ_A),
        advanced_mode: unpack(w, BL_ADV),
        premultiplied_src: unpack(w, BL_ADV_NPM) == 0,
        normalized_dst: unpack(w, BL_ADV_DST_FREE) == 0,
        overlap: unpack(w, BL_ADV_OVL),
    })
}

/// Reads the `NVNcolorState` at the guest address `s`.
pub(crate) fn color(s: u64) -> AbiResult<ColorState> {
    let w = load(s, COLOR)?;
    Ok(ColorState {
        blend_enable: unpack(w, COLOR_BLEND_EN) as u8,
        logic_op: unpack(w, COLOR_LOGIC_OP),
        alpha_func: unpack(w, COLOR_ALPHA_FUNC),
    })
}

/// Reads the `NVNchannelMaskState` at the guest address `s`.
pub(crate) fn channel_mask(s: u64) -> AbiResult<ChannelMaskState> {
    Ok(ChannelMaskState {
        mask: Obj(s).u32(0)?,
    })
}

/// Reads the `NVNdepthStencilState` at the guest address `s`.
pub(crate) fn depth_stencil(s: u64) -> AbiResult<DepthStencilState> {
    let w = load(s, DS)?;
    let face = |func, fail, zfail, zpass| StencilFace {
        func: unpack(w, func),
        fail: unpack(w, fail),
        zfail: unpack(w, zfail),
        zpass: unpack(w, zpass),
    };
    Ok(DepthStencilState {
        depth_test: unpack(w, DS_DT) != 0,
        depth_write: unpack(w, DS_DW) != 0,
        depth_func: unpack(w, DS_DF),
        stencil_test: unpack(w, DS_ST) != 0,
        front: face(DS_SF_F, DS_FAIL_F, DS_ZFAIL_F, DS_ZPASS_F),
        back: face(DS_SF_B, DS_FAIL_B, DS_ZFAIL_B, DS_ZPASS_B),
    })
}

/// Reads the `NVNmultisampleState` at the guest address `s`.
pub(crate) fn multisample(s: u64) -> AbiResult<MultisampleState> {
    Ok(MultisampleState {
        enable: byte(s, MS_ENABLE)? != 0,
        samples: byte(s, MS_SAMPLES)?,
        alpha_to_coverage: byte(s, MS_A2C)? != 0,
        alpha_to_coverage_dither: byte(s, MS_A2C_DITHER)? != 0,
        raster_samples: byte(s, MS_RASTER)?,
        coverage_modulation: byte(s, MS_COV_MOD)?,
        coverage_to_color: byte(s, MS_C2C_EN)? != 0,
        coverage_to_color_output: byte(s, MS_C2C_OUT)?,
        sample_locations: byte(s, MS_SL_EN)? != 0,
        sample_locations_grid: byte(s, MS_SL_GRID)? != 0,
    })
}

/// Reads the `NVNpolygonState` at the guest address `s`.
pub(crate) fn polygon(s: u64) -> AbiResult<PolygonState> {
    let w = load(s, POLY)?;
    Ok(PolygonState {
        cull_face: unpack(w, PO_CULL),
        front_face: unpack(w, PO_FRONT),
        mode: unpack(w, PO_MODE),
        offset_enables: unpack(w, PO_OFFSET),
    })
}

/// Reads the `NVNvertexAttribState` at the guest address `s`.
pub(crate) fn vertex_attrib(s: u64) -> AbiResult<VertexAttribState> {
    let w = load(s, VA)?;
    Ok(VertexAttribState {
        format: unpack(w, VA_FMT),
        offset: unpack(w, VA_OFFSET),
        stream: unpack(w, VA_STREAM),
    })
}

/// Reads the `NVNvertexStreamState` at the guest address `s`.
pub(crate) fn vertex_stream(s: u64) -> AbiResult<VertexStreamState> {
    Ok(VertexStreamState {
        stride: Obj(s).u32(VS_STRIDE)?,
        divisor: Obj(s).u32(VS_DIVISOR)?,
    })
}
