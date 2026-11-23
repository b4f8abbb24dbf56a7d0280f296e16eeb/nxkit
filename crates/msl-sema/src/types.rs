//! Resolved types and their memory layout (MSL rules, which match the AIR data layout).

use std::fmt;
use std::rc::Rc;

pub use msl_parse::ty::{Scalar, Space};

/// Index of a struct in [`crate::hir::Module::structs`].
pub type StructId = usize;

/// The dimension and kind of a texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TexKind {
    /// `texture1d`
    T1d,
    /// `texture_buffer`
    Buffer,
    /// `texture2d`
    T2d,
    /// `texture2d_array`
    T2dArray,
    /// `texture2d_ms`
    T2dMs,
    /// `texture2d_ms_array` (query-only).
    T2dMsArray,
    /// `texture3d`
    T3d,
    /// `texturecube`
    Cube,
    /// `texturecube_array`
    CubeArray,
    /// `depth2d`
    Depth2d,
    /// `depth2d_array`
    Depth2dArray,
    /// `depthcube`
    DepthCube,
    /// `depthcube_array`
    DepthCubeArray,
}

impl TexKind {
    /// Parses the MSL type name.
    pub fn from_name(s: &str) -> Option<TexKind> {
        Some(match s {
            "texture1d" => TexKind::T1d,
            "texture_buffer" => TexKind::Buffer,
            "texture2d" => TexKind::T2d,
            "texture2d_array" => TexKind::T2dArray,
            "texture2d_ms" => TexKind::T2dMs,
            "texture2d_ms_array" => TexKind::T2dMsArray,
            "texture3d" => TexKind::T3d,
            "texturecube" => TexKind::Cube,
            "texturecube_array" => TexKind::CubeArray,
            "depth2d" => TexKind::Depth2d,
            "depth2d_array" => TexKind::Depth2dArray,
            "depthcube" => TexKind::DepthCube,
            "depthcube_array" => TexKind::DepthCubeArray,
            _ => return None,
        })
    }

    /// The MSL type name.
    pub fn name(self) -> &'static str {
        match self {
            TexKind::T1d => "texture1d",
            TexKind::Buffer => "texture_buffer",
            TexKind::T2d => "texture2d",
            TexKind::T2dArray => "texture2d_array",
            TexKind::T2dMs => "texture2d_ms",
            TexKind::T2dMsArray => "texture2d_ms_array",
            TexKind::T3d => "texture3d",
            TexKind::Cube => "texturecube",
            TexKind::CubeArray => "texturecube_array",
            TexKind::Depth2d => "depth2d",
            TexKind::Depth2dArray => "depth2d_array",
            TexKind::DepthCube => "depthcube",
            TexKind::DepthCubeArray => "depthcube_array",
        }
    }

    /// The suffix AIR uses in handle type and intrinsic names (`2d_array`, `cube`, `depth_2d` ...).
    pub fn air_name(self) -> &'static str {
        match self {
            TexKind::T1d => "texture_1d",
            TexKind::Buffer => "texture_buffer_1d",
            TexKind::T2d => "texture_2d",
            TexKind::T2dArray => "texture_2d_array",
            TexKind::T2dMs => "texture_2d_ms",
            TexKind::T2dMsArray => "texture_2d_ms_array",
            TexKind::T3d => "texture_3d",
            TexKind::Cube => "texture_cube",
            TexKind::CubeArray => "texture_cube_array",
            TexKind::Depth2d => "depth_2d",
            TexKind::Depth2dArray => "depth_2d_array",
            TexKind::DepthCube => "depth_cube",
            TexKind::DepthCubeArray => "depth_cube_array",
        }
    }

    /// A depth texture (sampling returns a scalar).
    pub fn is_depth(self) -> bool {
        matches!(
            self,
            TexKind::Depth2d | TexKind::Depth2dArray | TexKind::DepthCube | TexKind::DepthCubeArray
        )
    }

    /// An array texture (sampling takes a slice index).
    pub fn is_array(self) -> bool {
        matches!(
            self,
            TexKind::T2dArray
                | TexKind::T2dMsArray
                | TexKind::CubeArray
                | TexKind::Depth2dArray
                | TexKind::DepthCubeArray
        )
    }

    /// A cube texture.
    pub fn is_cube(self) -> bool {
        matches!(
            self,
            TexKind::Cube | TexKind::CubeArray | TexKind::DepthCube | TexKind::DepthCubeArray
        )
    }

    /// Number of float coordinates for sampling.
    pub fn coord_dim(self) -> u8 {
        match self {
            TexKind::T1d | TexKind::Buffer => 1,
            TexKind::T2d
            | TexKind::T2dArray
            | TexKind::T2dMs
            | TexKind::T2dMsArray
            | TexKind::Depth2d
            | TexKind::Depth2dArray => 2,
            _ => 3,
        }
    }

    /// The same texture as a depth texture (`spvDepthCast`).
    pub fn to_depth(self) -> Option<TexKind> {
        Some(match self {
            TexKind::T2d => TexKind::Depth2d,
            TexKind::T2dArray => TexKind::Depth2dArray,
            TexKind::Cube => TexKind::DepthCube,
            TexKind::CubeArray => TexKind::DepthCubeArray,
            k if k.is_depth() => k,
            _ => return None,
        })
    }
}

/// Texture access qualifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Access {
    /// `access::sample` (the default)
    Sample,
    /// `access::read`
    Read,
    /// `access::write`
    Write,
    /// `access::read_write`
    ReadWrite,
}

impl Access {
    /// Parses the access name of the parser (`sample`, `access::write` ...).
    pub fn from_name(s: &str) -> Option<Access> {
        Some(match s.trim_start_matches("access::") {
            "sample" | "" => Access::Sample,
            "read" => Access::Read,
            "write" => Access::Write,
            "read_write" => Access::ReadWrite,
            _ => return None,
        })
    }

    /// The MSL spelling (`sample`, `read`, `write`, `read_write`).
    pub fn name(self) -> &'static str {
        match self {
            Access::Sample => "sample",
            Access::Read => "read",
            Access::Write => "write",
            Access::ReadWrite => "read_write",
        }
    }
}

/// A texture type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tex {
    /// Dimension and kind.
    pub kind: TexKind,
    /// Element (component) type: float, half, int, uint ...
    pub elem: Scalar,
    /// Access qualifier.
    pub access: Access,
}

/// A resolved type.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Type {
    /// `void`
    #[default]
    Void,
    /// A scalar (`size_t` is kept as its own scalar, laid out as `ulong`).
    Scalar(Scalar),
    /// `floatN`, `uintN` ... (`n` is 2 to 4).
    Vector(Scalar, u8),
    /// `packed_floatN` ...: no padding, aligned like the element.
    Packed(Scalar, u8),
    /// `floatCxR`: `cols` columns of `floatR`.
    Matrix(Scalar, u8, u8),
    /// A struct of the module.
    Struct(StructId),
    /// `T name[n]` or `spvUnsafeArray<T, n>`.
    Array(Rc<Type>, u32),
    /// A pointer into an address space.
    Ptr(Space, Rc<Type>),
    /// A texture handle.
    Texture(Tex),
    /// A sampler handle.
    Sampler,
}

/// Size and alignment in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Size including tail padding.
    pub size: u32,
    /// Alignment.
    pub align: u32,
}

/// Size in bytes of a scalar (`bool` takes one byte).
pub fn scalar_size(s: Scalar) -> u32 {
    s.size()
}

impl Type {
    /// `bool`
    pub const BOOL: Type = Type::Scalar(Scalar::Bool);
    /// `int`
    pub const INT: Type = Type::Scalar(Scalar::Int);
    /// `uint`
    pub const UINT: Type = Type::Scalar(Scalar::Uint);
    /// `float`
    pub const FLOAT: Type = Type::Scalar(Scalar::Float);

    /// A vector, or the scalar for width 1.
    pub fn vec(elem: Scalar, n: u8) -> Type {
        if n == 1 {
            Type::Scalar(elem)
        } else {
            Type::Vector(elem, n)
        }
    }

    /// The element scalar of a scalar, vector or packed vector.
    pub fn elem(&self) -> Option<Scalar> {
        match self {
            Type::Scalar(s) | Type::Vector(s, _) | Type::Packed(s, _) => Some(*s),
            _ => None,
        }
    }

    /// The number of components of a scalar (1), vector or packed vector.
    pub fn width(&self) -> Option<u8> {
        match self {
            Type::Scalar(_) => Some(1),
            Type::Vector(_, n) | Type::Packed(_, n) => Some(*n),
            _ => None,
        }
    }

    /// A scalar or (unpacked) vector.
    pub fn is_numeric(&self) -> bool {
        matches!(self, Type::Scalar(_) | Type::Vector(..))
    }

    /// The unpacked value type of a packed vector, else the type itself.
    pub fn unpacked(&self) -> Type {
        match self {
            Type::Packed(s, n) => Type::Vector(*s, *n),
            t => t.clone(),
        }
    }

    /// The same shape with another element type.
    pub fn with_elem(&self, s: Scalar) -> Type {
        match self {
            Type::Vector(_, n) => Type::Vector(s, *n),
            Type::Packed(_, n) => Type::Packed(s, *n),
            _ => Type::Scalar(s),
        }
    }
}

/// True for float and half.
pub fn is_float(s: Scalar) -> bool {
    matches!(s, Scalar::Half | Scalar::Float)
}

/// True for signed integers.
pub fn is_signed(s: Scalar) -> bool {
    matches!(s, Scalar::Char | Scalar::Short | Scalar::Int | Scalar::Long)
}

/// A struct definition with its layout.
#[derive(Clone, Debug)]
pub struct StructDef {
    /// Source name.
    pub name: Rc<str>,
    /// Fields in declaration order.
    pub fields: Vec<FieldDef>,
    /// Size and alignment.
    pub layout: Layout,
}

/// A struct field.
#[derive(Clone, Debug)]
pub struct FieldDef {
    /// Source name.
    pub name: Rc<str>,
    /// Field type.
    pub ty: Type,
    /// Attributes (`[[id(n)]]`, `[[color(n)]]`, `[[user(locn0)]]` ...).
    pub attrs: Vec<msl_parse::ast::Attr>,
    /// Byte offset.
    pub offset: u32,
    /// Declared `const` (a `device const T*` member is read-only).
    pub is_const: bool,
    /// The pointee must be accessed using volatile loads.
    pub is_volatile: bool,
    /// Declared as `metal::array<T, n>` (the type is an [`Type::Array`]).
    pub metal_array: bool,
}

/// Computes layouts, holds the module's struct definitions.
#[derive(Clone, Debug, Default)]
pub struct Types {
    /// All structs, indexed by [`StructId`].
    pub structs: Vec<StructDef>,
}

impl Types {
    /// Size and alignment of a type. `Void` has size 0.
    pub fn layout(&self, t: &Type) -> Layout {
        match t {
            Type::Void => Layout { size: 0, align: 1 },
            Type::Scalar(s) => {
                let n = scalar_size(*s);
                Layout { size: n, align: n }
            }
            Type::Vector(s, n) => {
                let e = scalar_size(*s);
                let n4 = if *n == 3 { 4 } else { *n as u32 };
                Layout {
                    size: e * n4,
                    align: e * n4,
                }
            }
            Type::Packed(s, n) => {
                let e = scalar_size(*s);
                Layout {
                    size: e * *n as u32,
                    align: e,
                }
            }
            Type::Matrix(s, c, r) => {
                let col = self.layout(&Type::Vector(*s, *r));
                Layout {
                    size: col.size * *c as u32,
                    align: col.align,
                }
            }
            Type::Struct(id) => self.structs[*id].layout,
            Type::Array(e, n) => {
                let l = self.layout(e);
                Layout {
                    size: l.size * *n,
                    align: l.align,
                }
            }
            Type::Ptr(..) | Type::Texture(_) | Type::Sampler => Layout { size: 8, align: 8 },
        }
    }

    /// Lays out a struct from its fields (C rules) and registers it.
    pub fn add_struct(
        &mut self,
        name: Rc<str>,
        fields: Vec<(Rc<str>, Type, Vec<msl_parse::ast::Attr>, bool)>,
    ) -> StructId {
        let mut off = 0u32;
        let mut align = 1u32;
        let mut out = Vec::with_capacity(fields.len());
        for (fname, ty, attrs, is_const) in fields {
            let l = self.layout(&ty);
            off = off.div_ceil(l.align) * l.align;
            out.push(FieldDef {
                name: fname,
                ty,
                attrs,
                offset: off,
                is_const,
                is_volatile: false,
                metal_array: false,
            });
            off += l.size;
            align = align.max(l.align);
        }
        let size = off.div_ceil(align) * align;
        self.structs.push(StructDef {
            name,
            fields: out,
            layout: Layout { size, align },
        });
        self.structs.len() - 1
    }

    /// The MSL spelling of a type, as AIR metadata writes it (`float4`, `texture2d<float, sample>`).
    pub fn msl_name(&self, t: &Type) -> String {
        match t {
            Type::Void => "void".into(),
            Type::Scalar(s) => scalar_msl(*s).into(),
            Type::Vector(s, n) => format!("{}{}", scalar_msl(*s), n),
            Type::Packed(s, n) => format!("packed_{}{}", scalar_msl(*s), n),
            Type::Matrix(s, c, r) => format!("{}{}x{}", scalar_msl(*s), c, r),
            Type::Struct(id) => self.structs[*id].name.to_string(),
            Type::Array(e, n) => format!("{}[{}]", self.msl_name(e), n),
            Type::Ptr(sp, e) => format!("{} {}*", sp.name(), self.msl_name(e)),
            Type::Texture(tx) => format!(
                "{}<{}, {}>",
                tx.kind.name(),
                scalar_msl(tx.elem),
                tx.access.name()
            ),
            Type::Sampler => "sampler".into(),
        }
    }

    /// The MSL spelling of a field's type (`array<texture2d<float, sample>, 1>` for a
    /// `metal::array` member).
    pub fn field_msl_name(&self, f: &FieldDef) -> String {
        match &f.ty {
            Type::Array(e, n) if f.metal_array => format!("array<{}, {}>", self.msl_name(e), n),
            t => self.msl_name(t),
        }
    }

    /// Number of argument-buffer ids a member of type `t` takes (data: 1, a struct of resources:
    /// the sum of its members, arrays of such structs: per element).
    pub fn id_count(&self, t: &Type) -> u32 {
        match t {
            Type::Struct(id) if self.is_argument_buffer(*id) => self.structs[*id]
                .fields
                .iter()
                .map(|f| self.id_count(&f.ty))
                .sum(),
            Type::Array(e, n) if self.holds_resource(e) => n * self.id_count(e),
            _ => 1,
        }
    }

    /// True when a struct (recursively) holds texture, sampler or pointer members: an argument buffer.
    pub fn is_argument_buffer(&self, id: StructId) -> bool {
        self.structs[id]
            .fields
            .iter()
            .any(|f| self.holds_resource(&f.ty))
    }

    fn holds_resource(&self, t: &Type) -> bool {
        match t {
            Type::Ptr(..) | Type::Texture(_) | Type::Sampler => true,
            Type::Array(e, _) => self.holds_resource(e),
            Type::Struct(id) => self.is_argument_buffer(*id),
            _ => false,
        }
    }
}

/// The MSL name of a scalar (`size_t` is spelled `ulong`).
pub fn scalar_msl(s: Scalar) -> &'static str {
    match s {
        Scalar::SizeT => "ulong",
        s => s.name(),
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Struct(id) => write!(f, "struct#{id}"),
            t => write!(f, "{}", Types::default().msl_name(t)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_and_packed_layouts() {
        let t = Types::default();
        assert_eq!(
            t.layout(&Type::Vector(Scalar::Float, 3)),
            Layout {
                size: 16,
                align: 16
            }
        );
        assert_eq!(
            t.layout(&Type::Vector(Scalar::Half, 2)),
            Layout { size: 4, align: 4 }
        );
        assert_eq!(
            t.layout(&Type::Packed(Scalar::Uint, 3)),
            Layout { size: 12, align: 4 }
        );
        assert_eq!(
            t.layout(&Type::Scalar(Scalar::Bool)),
            Layout { size: 1, align: 1 }
        );
    }

    #[test]
    fn struct_layout_follows_c_rules() {
        let mut t = Types::default();
        let fields = vec![
            ("a".into(), Type::Packed(Scalar::Uint, 3), vec![], false),
            ("b".into(), Type::UINT, vec![], false),
            ("c".into(), Type::Packed(Scalar::Uint, 2), vec![], false),
            ("d".into(), Type::Packed(Scalar::Uint, 4), vec![], false),
            ("e".into(), Type::BOOL, vec![], false),
            ("h".into(), Type::Vector(Scalar::Half, 2), vec![], false),
            ("f3".into(), Type::Vector(Scalar::Float, 3), vec![], false),
        ];
        let p = t.add_struct("P".into(), fields);
        let offs: Vec<u32> = t.structs[p].fields.iter().map(|f| f.offset).collect();
        assert_eq!(offs, vec![0, 12, 16, 24, 40, 44, 48]);
        assert_eq!(
            t.structs[p].layout,
            Layout {
                size: 64,
                align: 16
            }
        );
        let q = t.add_struct(
            "Q".into(),
            vec![
                (
                    "p".into(),
                    Type::Array(Rc::new(Type::Struct(p)), 2),
                    vec![],
                    false,
                ),
                ("x".into(), Type::UINT, vec![], false),
            ],
        );
        assert_eq!(t.structs[q].layout.size, 144);
    }

    #[test]
    fn metadata_names() {
        let t = Types::default();
        let tx = Tex {
            kind: TexKind::T2d,
            elem: Scalar::Float,
            access: Access::Sample,
        };
        assert_eq!(t.msl_name(&Type::Texture(tx)), "texture2d<float, sample>");
        assert_eq!(t.msl_name(&Type::Vector(Scalar::Uint, 4)), "uint4");
    }
}
