//! Itanium names of internal helpers as clang spells them (`_ZL13dm_alpha_passfRU11MTLconstantK14dm_alpha_state`),
//! with the address space as a vendor qualifier and substitutions (`S_`, `S0_` ...).

use msl_sema::hir::Func;
use msl_sema::types::{Scalar, Space, Type, Types};

fn scalar_code(s: Scalar) -> &'static str {
    match s {
        Scalar::Bool => "b",
        Scalar::Char => "c",
        Scalar::Uchar => "h",
        Scalar::Short => "s",
        Scalar::Ushort => "t",
        Scalar::Int => "i",
        Scalar::Uint => "j",
        Scalar::Long => "l",
        Scalar::Ulong => "m",
        Scalar::SizeT => "m",
        Scalar::Half => "Dh",
        Scalar::Float => "f",
    }
}

fn space_name(sp: Space) -> &'static str {
    match sp {
        Space::Device => "MTLdevice",
        Space::Constant => "MTLconstant",
        Space::Thread => "MTLthread",
        Space::Threadgroup => "MTLthreadgroup",
    }
}

fn seq_id(k: usize) -> String {
    if k == 0 {
        return "S_".into();
    }
    let mut n = k - 1;
    let digits = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut s = Vec::new();
    loop {
        s.push(digits[n % 36]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    s.reverse();
    format!("S{}_", String::from_utf8_lossy(&s))
}

struct M<'a> {
    types: &'a Types,
    subs: Vec<String>,
}

impl M<'_> {
    /// Emits a substitutable component: the substitution if seen, else the text (and records it).
    fn sub(&mut self, text: String) -> String {
        if let Some(k) = self.subs.iter().position(|s| *s == text) {
            return seq_id(k);
        }
        self.subs.push(text.clone());
        text
    }

    fn ty(&mut self, t: &Type) -> Option<String> {
        Some(match t {
            Type::Scalar(s) => scalar_code(*s).to_string(),
            Type::Vector(s, n) | Type::Packed(s, n) => {
                let full = format!("Dv{n}_{}", scalar_code(*s));
                self.sub(full)
            }
            Type::Struct(id) => {
                let n = &self.types.structs[*id].name;
                self.sub(format!("{}{n}", n.len()))
            }
            Type::Ptr(sp, inner) => {
                let q = self.qualified(*sp, inner, false)?;
                self.sub(format!("P{q}"))
            }
            _ => return None,
        })
    }

    /// `U<len>MTL<space>[K]<type>` as one substitutable unit.
    fn qualified(&mut self, sp: Space, t: &Type, is_const: bool) -> Option<String> {
        let plain = self.plain(t)?;
        let text = format!(
            "U{}{}{}{}",
            space_name(sp).len(),
            space_name(sp),
            if is_const { "K" } else { "" },
            plain
        );
        if let Some(k) = self.subs.iter().position(|s| *s == text) {
            return Some(seq_id(k));
        }
        let inner = self.ty(t)?;
        let out = format!(
            "U{}{}{}{}",
            space_name(sp).len(),
            space_name(sp),
            if is_const { "K" } else { "" },
            inner
        );
        self.subs.push(text);
        Some(out)
    }

    /// The unsubstituted spelling, used as the substitution key.
    fn plain(&self, t: &Type) -> Option<String> {
        Some(match t {
            Type::Scalar(s) => scalar_code(*s).to_string(),
            Type::Vector(s, n) | Type::Packed(s, n) => format!("Dv{n}_{}", scalar_code(*s)),
            Type::Struct(id) => {
                let n = &self.types.structs[*id].name;
                format!("{}{n}", n.len())
            }
            Type::Ptr(..) => return None,
            _ => return None,
        })
    }
}

fn function_encoding(f: &Func, types: &Types) -> Option<String> {
    let mut m = M {
        types,
        subs: Vec::new(),
    };
    let mut params = String::new();
    for p in &f.params {
        match p.by_ref {
            Some(sp) => {
                let is_const = p.is_const || sp == Space::Constant;
                let key = format!(
                    "RU{}{}{}{}",
                    space_name(sp).len(),
                    space_name(sp),
                    if is_const { "K" } else { "" },
                    m.plain(&p.ty)?
                );
                if let Some(k) = m.subs.iter().position(|s| *s == key) {
                    params.push_str(&seq_id(k));
                } else {
                    let q = m.qualified(sp, &p.ty, is_const)?;
                    params.push('R');
                    params.push_str(&q);
                    m.subs.push(key);
                }
            }
            None => params.push_str(&m.ty(&p.ty)?),
        }
    }
    if params.is_empty() {
        params.push('v');
    }
    Some(format!("{}{}{params}", f.name.len(), f.name))
}

/// clang's name of an internal (static) helper, or None for a parameter type this mangler does not know.
pub fn helper_name(f: &Func, types: &Types) -> Option<String> {
    Some(format!("_ZL{}", function_encoding(f, types)?))
}

/// Itanium local entity name for a named static threadgroup allocation.
pub fn threadgroup_local(f: &Func, types: &Types, name: &str) -> Option<String> {
    Some(format!(
        "_ZZ{}E{}{name}",
        function_encoding(f, types)?,
        name.len()
    ))
}

/// clang's name of an engine template instantiation whose spelling this mangler knows:
/// `uint dm_pool_mark<P>(constant P*, uint)`.
pub fn template_name(f: &Func, types: &Types) -> Option<String> {
    if !f.template || &*f.name != "dm_pool_mark" {
        return None;
    }
    let Some(Type::Ptr(Space::Constant, p)) = f.params.first().map(|p| &p.ty) else {
        return None;
    };
    let Type::Struct(id) = **p else {
        return None;
    };
    let n = &types.structs[id].name;
    Some(format!(
        "_ZL12dm_pool_markI{}{n}EjPU11MTLconstantKT_j",
        n.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::seq_id;

    #[test]
    fn substitution_ids() {
        assert_eq!(seq_id(0), "S_");
        assert_eq!(seq_id(1), "S0_");
        assert_eq!(seq_id(11), "SA_");
        assert_eq!(seq_id(37), "S10_");
    }
}
