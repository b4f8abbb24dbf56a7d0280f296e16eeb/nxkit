//! The `limits` list of `game.json`: the frame limit rules of the family and the title module (`RuntimeHooks::limits`),
//! which the host maps into the `LimitsConfig` of `nxkit-svc-pacing` (R30b). The host knows no engine here: a rule names
//! a place of one build, a fixups key of the main module or a reflected class, and the pacing service writes the target
//! frame rate there.

use crate::error::Result;
use crate::fields::{bad, field, num, text};
use crate::json::Value;
use nxkit_svc_pacing::limits::rows::Kind;
use nxkit_svc_pacing::limits::{Layout, Module, ReflectSpec, RowSpec, Site};

/// The rules of `game.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LimitRules {
    /// The rows, family rules first.
    pub rows: Vec<RowSpec>,
    /// The reflected class, the first one listed.
    pub reflected: Option<ReflectSpec>,
}

/// The rules are read once per run, so the few names they hold live for the run.
fn name(v: &Value, key: &str, ctx: &str) -> Result<&'static str> {
    Ok(Box::leak(text(v, key, ctx)?.into_boxed_str()))
}

fn u32_of(v: &Value, key: &str, ctx: &str) -> Result<u32> {
    u32::try_from(num(v, key, ctx)?).map_err(|_| bad(format!("{ctx}: \"{key}\" is out of range")))
}

fn kind(v: &Value) -> Result<Kind> {
    let ctx = "limits.kind";
    Ok(match text(v, "id", ctx)?.as_str() {
        "fps_f32" => Kind::FpsF32,
        "vsyncs_i32" => Kind::VsyncsI32,
        "ms_f32" => Kind::MsF32,
        "fps_i32" => Kind::FpsI32,
        "ticks_u32" => Kind::TicksU32 {
            scale: u32_of(v, "scale", ctx)?,
        },
        "secs_f32" => Kind::SecsF32,
        "mode_u32" => Kind::ModeU32,
        "fps_f64" => Kind::FpsF64,
        "interval_minus_one_u32" => Kind::IntervalMinusOneU32,
        "pin_u32" => Kind::PinU32 {
            from: u32_of(v, "from", ctx)?,
            to: u32_of(v, "to", ctx)?,
        },
        other => return Err(bad(format!("{ctx}: unknown kind \"{other}\""))),
    })
}

fn row(v: &Value) -> Result<RowSpec> {
    let ctx = "limits";
    let s = field(v, "site", ctx)?;
    let site = if s.get("found").is_some() {
        Site::Found {
            key: name(s, "found", "limits.site")?,
            add: num(s, "add", "limits.site")?,
        }
    } else {
        Site::Fixed {
            ptr_off: num(s, "ptr_off", "limits.site")?,
            field: num(s, "field", "limits.site")?,
        }
    };
    Ok(RowSpec {
        what: name(v, "what", ctx)?,
        build: match v.get("build") {
            Some(_) => Some(name(v, "build", ctx)?),
            None => None,
        },
        site,
        kind: kind(field(v, "kind", ctx)?)?,
        copies: u32_of(v, "copies", ctx)?,
    })
}

fn reflected(v: &Value) -> Result<ReflectSpec> {
    let ctx = "limits.reflected";
    Ok(ReflectSpec {
        class: name(v, "class", ctx)?,
        int_field: name(v, "int_field", ctx)?,
        float_field: name(v, "float_field", ctx)?,
        layout: Layout {
            record_size: num(v, "record_size", ctx)?,
            fields_at: num(v, "fields_at", ctx)?,
            field_stride: num(v, "field_stride", ctx)?,
            field_offset_at: num(v, "field_offset_at", ctx)?,
            instance_type_at: num(v, "instance_type_at", ctx)?,
        },
    })
}

impl LimitRules {
    /// Reads the `limits` list. A missing list means no rules.
    pub fn parse(v: Option<&Value>) -> Result<LimitRules> {
        let mut out = LimitRules::default();
        let Some(list) = v else { return Ok(out) };
        let items = list
            .as_array()
            .ok_or_else(|| bad("\"limits\" is not an array"))?;
        for e in items {
            match e.get("reflected") {
                Some(r) => {
                    let spec = reflected(r)?;
                    out.reflected.get_or_insert(spec);
                }
                None => out.rows.push(row(e)?),
            }
        }
        Ok(out)
    }

    /// Whether there is nothing to write.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.reflected.is_none()
    }

    /// The fixups keys of the rows that the rewriter finds by name.
    pub fn found_keys(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.rows.iter().filter_map(|r| match r.site {
            Site::Found { key, .. } => Some(key),
            Site::Fixed { .. } => None,
        })
    }
}

/// The main module as the limits see it: its address, image, build id (the first 16 bytes, as the rows name it) and the
/// offsets that `fixups` holds for `keys`. `fixup` reads one value of the fixups of the main module.
pub fn module_of(
    main: &crate::module::Module,
    keys: impl Iterator<Item = &'static str>,
    fixup: impl Fn(&str) -> Option<u64>,
) -> Module {
    let found = keys
        .filter_map(|k| {
            fixup(k).map(|off| nxkit_svc_pacing::limits::Found {
                key: k.to_owned(),
                off,
            })
        })
        .collect();
    Module {
        base: main.base,
        size: main.size,
        build_id: main
            .build_id
            .chars()
            .take(32)
            .collect::<String>()
            .to_ascii_lowercase(),
        text_end: main.text.offset + main.text.size,
        ro_off: main.rodata.offset,
        back_off: 0,
        found,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(src: &str) -> Result<LimitRules> {
        let v = crate::json::parse(src)?;
        LimitRules::parse(Some(&v))
    }

    #[test]
    fn a_found_row_and_a_fixed_row_are_read() {
        let r = rules(
            r#"[{"what": "a limit", "site": {"found": "NAME:limit", "add": 16},
                 "kind": {"id": "fps_f32"}, "copies": 2},
                {"what": "w", "build": "ab", "site": {"ptr_off": 8, "field": 4},
                 "kind": {"id": "ticks_u32", "scale": 19200000}, "copies": 1}]"#,
        )
        .unwrap();
        assert_eq!(
            r.rows[0],
            RowSpec {
                what: "a limit",
                build: None,
                site: Site::Found {
                    key: "NAME:limit",
                    add: 16
                },
                kind: Kind::FpsF32,
                copies: 2,
            }
        );
        assert_eq!(r.rows[1].build, Some("ab"));
        assert_eq!(r.rows[1].kind, Kind::TicksU32 { scale: 19_200_000 });
        assert_eq!(r.found_keys().collect::<Vec<_>>(), ["NAME:limit"]);
        assert!(r.reflected.is_none());
    }

    #[test]
    fn no_list_is_no_rule_and_an_unknown_kind_is_an_error() {
        assert!(LimitRules::parse(None).unwrap().is_empty());
        assert!(
            rules(r#"[{"what": "w", "site": {"ptr_off": 0, "field": 4}, "kind": {"id": "x"}, "copies": 1}]"#)
                .is_err()
        );
    }
}
