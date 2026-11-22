//! The `temporal_upscale` key of `settings` (R72): the draws of the game that the encoder replaces by MetalFX temporal
//! upscaling, and how it reads their inputs. The host types it here and `frames.rs` hands it to `nxkit-gpu`. A key
//! that starts with `_` is a note and goes unread.

use crate::error::Result;
use crate::fields::{bad, field, num, text};
use crate::json::Value;

const CTX: &str = "settings.temporal_upscale";

/// The stage of a uniform buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UboStage {
    /// The vertex stage.
    Vertex,
    /// The fragment stage.
    Fragment,
}

/// A draw by the shader key of its fragment program and one of its fragment texture slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrawTexture {
    /// The shader key of the fragment program.
    pub fragment: String,
    /// The fragment texture slot.
    pub slot: u32,
}

/// The draw that gives the depth, the velocity and the view uniform buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The shader key of the fragment program.
    pub fragment: String,
    /// The fragment texture slot of the depth.
    pub depth_slot: u32,
    /// The fragment texture slot of the velocity.
    pub velocity_slot: u32,
    /// The stage and the slot of the view uniform buffer.
    pub view: (UboStage, u32),
}

/// The values of `temporal_upscale`. `nxkit_gpu::TemporalUpscale` describes each one.
#[derive(Debug, Clone, PartialEq)]
pub struct TemporalUpscale {
    /// `upscale`: `{ "fragment", "color_slot" }`.
    pub upscale: DrawTexture,
    /// `source`: `{ "fragment", "depth_slot", "velocity_slot", "view": { "stage", "slot" } }`.
    pub source: Source,
    /// `passthrough`: a list of `{ "fragment", "slot" }`.
    pub passthrough: Vec<DrawTexture>,
    /// `view`: the byte offsets `clip_to_prev_clip`, `jitter` and `view_size` in the view uniform buffer.
    pub view: [u32; 3],
    /// `velocity.scale`.
    pub velocity_scale: [f32; 2],
    /// `velocity.bias`.
    pub velocity_bias: [f32; 2],
    /// `velocity.y_up`.
    pub velocity_y_up: bool,
    /// `velocity.zero_means_camera`.
    pub zero_means_camera: bool,
    /// `depth_reversed`.
    pub depth_reversed: bool,
}

fn small(v: &Value, key: &str, ctx: &str) -> Result<u32> {
    u32::try_from(num(v, key, ctx)?).map_err(|_| bad(format!("{ctx}.{key} is out of range")))
}

fn flag(v: &Value, key: &str, ctx: &str) -> Result<bool> {
    field(v, key, ctx)?
        .as_bool()
        .ok_or_else(|| bad(format!("{ctx}.{key} is not a boolean")))
}

/// Two numbers, as `[x, y]`.
fn pair(v: &Value, key: &str, ctx: &str) -> Result<[f32; 2]> {
    let not = || bad(format!("{ctx}.{key} is not a list of two numbers"));
    let a = field(v, key, ctx)?.as_array().ok_or_else(not)?;
    let n = |x: &Value| match x {
        Value::Int(i) => Some(*i as f32),
        Value::Float(f) => Some(*f as f32),
        _ => None,
    };
    match a {
        [x, y] => Ok([n(x).ok_or_else(not)?, n(y).ok_or_else(not)?]),
        _ => Err(not()),
    }
}

fn draw(v: &Value, slot: &str, ctx: &str) -> Result<DrawTexture> {
    Ok(DrawTexture {
        fragment: text(v, "fragment", ctx)?,
        slot: small(v, slot, ctx)?,
    })
}

/// Reads the `temporal_upscale` object.
pub fn parse(v: &Value) -> Result<TemporalUpscale> {
    let up = field(v, "upscale", CTX)?;
    let src = field(v, "source", CTX)?;
    let ctx_src = format!("{CTX}.source");
    let ubo = field(src, "view", &ctx_src)?;
    let ctx_ubo = format!("{ctx_src}.view");
    let stage = match text(ubo, "stage", &ctx_ubo)?.as_str() {
        "vertex" => UboStage::Vertex,
        "fragment" => UboStage::Fragment,
        other => {
            return Err(bad(format!(
                "{ctx_ubo}.stage {other:?} is not vertex or fragment"
            )));
        }
    };
    let passthrough = match v.get("passthrough") {
        None | Some(Value::Null) => Vec::new(),
        Some(list) => list
            .as_array()
            .ok_or_else(|| bad(format!("{CTX}.passthrough is not a list")))?
            .iter()
            .map(|e| draw(e, "slot", &format!("{CTX}.passthrough")))
            .collect::<Result<_>>()?,
    };
    let view = field(v, "view", CTX)?;
    let ctx_view = format!("{CTX}.view");
    let vel = field(v, "velocity", CTX)?;
    let ctx_vel = format!("{CTX}.velocity");
    Ok(TemporalUpscale {
        upscale: draw(up, "color_slot", &format!("{CTX}.upscale"))?,
        source: Source {
            fragment: text(src, "fragment", &ctx_src)?,
            depth_slot: small(src, "depth_slot", &ctx_src)?,
            velocity_slot: small(src, "velocity_slot", &ctx_src)?,
            view: (stage, small(ubo, "slot", &ctx_ubo)?),
        },
        passthrough,
        view: [
            small(view, "clip_to_prev_clip", &ctx_view)?,
            small(view, "jitter", &ctx_view)?,
            small(view, "view_size", &ctx_view)?,
        ],
        velocity_scale: pair(vel, "scale", &ctx_vel)?,
        velocity_bias: pair(vel, "bias", &ctx_vel)?,
        velocity_y_up: flag(vel, "y_up", &ctx_vel)?,
        zero_means_camera: flag(vel, "zero_means_camera", &ctx_vel)?,
        depth_reversed: flag(v, "depth_reversed", CTX)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    const SAMPLE: &str = r#"{
        "upscale": { "fragment": "be89b1ada1308881", "color_slot": 0 },
        "source": { "fragment": "cc0fa26ed24e4da1", "depth_slot": 3, "velocity_slot": 0,
                    "view": { "stage": "fragment", "slot": 0 } },
        "passthrough": [ { "fragment": "cc0fa26ed24e4da1", "slot": 2 }, { "fragment": "befb9b8e18bd3e4f", "slot": 2 } ],
        "view": { "clip_to_prev_clip": 1760, "jitter": 1824, "view_size": 1888 },
        "velocity": { "_note": "V = raw * scale + bias", "scale": [4.008, 4.008], "bias": [-2.00397, -2.00397],
                      "y_up": true, "zero_means_camera": true },
        "depth_reversed": true
    }"#;

    fn read(src: &str) -> Result<TemporalUpscale> {
        parse(&json::parse(src)?)
    }

    #[test]
    fn reads_every_value() {
        let t = read(SAMPLE).unwrap();
        assert_eq!(t.upscale.fragment, "be89b1ada1308881");
        assert_eq!(t.upscale.slot, 0);
        assert_eq!(t.source.depth_slot, 3);
        assert_eq!(t.source.view, (UboStage::Fragment, 0));
        assert_eq!(t.passthrough.len(), 2);
        assert_eq!(t.passthrough[1].fragment, "befb9b8e18bd3e4f");
        assert_eq!(t.view, [1760, 1824, 1888]);
        assert_eq!(t.velocity_scale, [4.008, 4.008]);
        assert_eq!(t.velocity_bias, [-2.00397, -2.00397]);
        assert!(t.velocity_y_up && t.zero_means_camera && t.depth_reversed);
    }

    #[test]
    fn refuses_a_missing_or_wrong_value() {
        assert!(
            read(&SAMPLE.replace("\"depth_reversed\": true", "\"depth_reversed\": 1")).is_err()
        );
        assert!(read(&SAMPLE.replace("\"stage\": \"fragment\"", "\"stage\": \"pixel\"")).is_err());
        assert!(read(&SAMPLE.replace("[4.008, 4.008]", "[4.008]")).is_err());
        assert!(read(&SAMPLE.replace("\"jitter\": 1824, ", "")).is_err());
    }

    #[test]
    fn no_pass_through_list_is_an_empty_one() {
        let start = SAMPLE.find("\"passthrough\"").unwrap();
        let end = SAMPLE.find("\"view\": { \"clip").unwrap();
        let t = read(&format!("{}{}", &SAMPLE[..start], &SAMPLE[end..])).unwrap();
        assert!(t.passthrough.is_empty());
    }
}
