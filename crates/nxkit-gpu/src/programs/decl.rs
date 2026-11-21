//! What a translated shader declares for a texture or image member: its texture type and its image access.

use super::{ImgAccess, ImgDecl};

/// Reads the access and the texel type from an MSL type such as `texture2d<uint, access::read_write>`.
pub(super) fn declared_img(msl_type: &str, atomic: bool) -> ImgDecl {
    let args = msl_type.split_once('<').map_or("", |(_, r)| r);
    let access = if args.contains("access::read_write") {
        ImgAccess::ReadWrite
    } else if args.contains("access::write") {
        ImgAccess::Write
    } else {
        ImgAccess::Read
    };
    let signed = args.trim_start().starts_with("int") || args.trim_start().starts_with("short");
    let buffer = msl_type.starts_with("texture_buffer");
    ImgDecl {
        access,
        atomic,
        signed,
        buffer,
    }
}

/// The texture type of an MSL argument type such as `texture2d_array<float>`.
pub(super) fn declared_dim(msl_type: &str) -> Option<nxkit_metal::TextureKind> {
    use nxkit_metal::TextureKind as K;
    let base = msl_type.split('<').next()?.trim();
    Some(match base {
        "texture2d" | "depth2d" | "texture1d" => K::D2,
        "texture2d_array" | "depth2d_array" | "texture1d_array" => K::D2Array,
        "texturecube" | "depthcube" => K::Cube,
        "texturecube_array" | "depthcube_array" => K::CubeArray,
        "texture3d" => K::D3,
        // An image only: a texture buffer over the guest bytes (`ImgDecl::buffer`).
        "texture_buffer" => K::D2,
        _ => return None,
    })
}
