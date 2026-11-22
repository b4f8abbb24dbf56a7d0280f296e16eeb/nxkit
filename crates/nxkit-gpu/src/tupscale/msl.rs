//! The shaders of the temporal upscale: the motion kernel and the copy draw.
//!
//! The motion kernel writes the motion of each input pixel as MetalFX takes it (`math.rs` has the same arithmetic):
//! the decoded velocity, or the camera motion from the depth and the matrix where the velocity texture has no value.
//! The copy draw writes a texture one to one into a colour attachment of another format.

use super::math::{Rows, Signs};
use crate::settings::VelocityCode;

pub(super) const LIBRARY: &str = "nxkit-tupscale";
pub(super) const MOTION: &str = "nxkit_tupscale_motion";
pub(super) const COPY_VS: &str = "nxkit_tupscale_vs";
pub(super) const COPY_FS: &str = "nxkit_tupscale_copy";

/// The bytes of `MotionParams`.
pub(super) const PARAMS_BYTES: usize = 112;

pub(super) const MSL: &str = r"
#include <metal_stdlib>
using namespace metal;

struct MotionParams {
    float4 rows[4];
    float2 size;
    float2 vscale;
    float2 vbias;
    float2 sign;
    float row_sign;
    uint y_up;
    uint zero_camera;
    uint pad;
};

kernel void nxkit_tupscale_motion(
    depth2d<float, access::read> depth [[texture(0)]],
    texture2d<float, access::read> velocity [[texture(1)]],
    texture2d<float, access::write> motion [[texture(2)]],
    constant MotionParams& p [[buffer(0)]],
    uint2 gid [[thread_position_in_grid]])
{
    if (float(gid.x) >= p.size.x || float(gid.y) >= p.size.y) {
        return;
    }
    float2 ndc = float2((float(gid.x) + 0.5) / p.size.x * 2.0 - 1.0,
                        (1.0 - (float(gid.y) + 0.5) / p.size.y * 2.0) * p.row_sign);
    float2 raw = velocity.read(gid).xy;
    float2 v;
    if (p.zero_camera != 0u && raw.x == 0.0 && raw.y == 0.0) {
        float4 c = float4(ndc, depth.read(gid), 1.0);
        float4 prev = c.x * p.rows[0] + c.y * p.rows[1] + c.z * p.rows[2] + c.w * p.rows[3];
        v = abs(prev.w) > 1e-20 ? ndc - prev.xy / prev.w : float2(0.0);
    } else {
        v = raw * p.vscale + p.vbias;
        if (p.y_up == 0u) {
            v.y = -v.y;
        }
    }
    float2 mv = float2(-v.x * p.size.x * 0.5, v.y * p.size.y * 0.5 * p.row_sign) * p.sign;
    motion.write(float4(mv, 0.0, 0.0), gid);
}

struct CopyV { float4 pos [[position]]; };

vertex CopyV nxkit_tupscale_vs(uint id [[vertex_id]]) {
    float2 p = float2((id << 1) & 2, id & 2);
    CopyV v;
    v.pos = float4(p * 2.0 - 1.0, 0.0, 1.0);
    return v;
}

fragment float4 nxkit_tupscale_copy(CopyV in [[stage_in]], texture2d<float, access::read> t [[texture(0)]]) {
    return t.read(uint2(in.pos.xy));
}
";

/// The bytes of `MotionParams` for a picture of `size` pixels.
pub(super) fn params(
    m: &Rows,
    size: [u32; 2],
    code: &VelocityCode,
    signs: &Signs,
    row_sign: f32,
) -> Vec<u8> {
    let mut f: Vec<f32> = m.iter().flatten().copied().collect();
    f.extend([size[0] as f32, size[1] as f32]);
    f.extend(code.scale);
    f.extend(code.bias);
    f.extend(signs.motion);
    f.push(row_sign);
    let mut b: Vec<u8> = f.iter().flat_map(|v| v.to_le_bytes()).collect();
    for word in [u32::from(code.y_up), u32::from(code.zero_means_camera), 0] {
        b.extend(word.to_le_bytes());
    }
    debug_assert_eq!(b.len(), PARAMS_BYTES);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parameters_fill_the_struct_of_the_kernel() {
        let code = VelocityCode {
            scale: [2.0; 2],
            bias: [-1.0; 2],
            y_up: true,
            zero_means_camera: true,
        };
        let b = params(&[[0.0; 4]; 4], [8, 4], &code, &Signs::PLAIN, -1.0);
        assert_eq!(b.len(), PARAMS_BYTES);
        assert_eq!(&b[64..68], &8.0f32.to_le_bytes(), "size");
        assert_eq!(&b[96..100], &(-1.0f32).to_le_bytes(), "row_sign");
        assert_eq!(&b[100..104], &1u32.to_le_bytes(), "y_up");
        for entry in [MOTION, COPY_VS, COPY_FS] {
            assert!(MSL.contains(&format!(" {entry}(")), "{entry}");
        }
    }
}
