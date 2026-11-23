//! What the reflection of a library says about transform feedback and geometry kernels (`crate::feedback`).

use crate::json::Json;

/// The geometry of a kernel `_bg<t>` (`geometry` of its `.json`, `nxb-xlate` `msl::geom`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Geometry {
    /// The most vertices one invocation emits.
    pub(crate) vertices: u32,
    /// The invocations per primitive.
    pub(crate) invocations: u32,
    /// The output topology: 1 points, 6 line strip, 7 triangle strip.
    pub(crate) output: u32,
    /// The words of the index list of one invocation.
    pub(crate) index_stride: u32,
    /// The words of one emitted vertex.
    pub(crate) stride: u32,
}

/// The capture facts of a library.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Facts {
    /// The entry that captures the vertex outputs for a geometry kernel (`entry_geometry_capture`, `main0_gs`).
    pub(crate) entry_gs: Option<String>,
    /// The transform feedback entry (`entry_tf`, `main0_tf`).
    pub(crate) entry_tf: Option<String>,
    /// The bytes of a record in each feedback buffer, 0 for a buffer without capture (`tf_strides`).
    pub(crate) tf_strides: [u32; 4],
    /// The geometry of a kernel.
    pub(crate) geometry: Option<Geometry>,
}

impl Facts {
    pub(crate) fn read(json: &Json) -> Self {
        let text = |k: &str| json.get(k).and_then(Json::as_str).map(str::to_owned);
        let mut tf_strides = [0; 4];
        if let Some(s) = json.get("tf_strides") {
            for (o, v) in tf_strides.iter_mut().zip(s.as_arr()) {
                *o = v.as_i64().and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
            }
        }
        let geometry = json.get("geometry").and_then(|g| {
            let n = |k: &str| {
                g.get(k)
                    .and_then(Json::as_i64)
                    .and_then(|v| u32::try_from(v).ok())
            };
            Some(Geometry {
                vertices: n("vertices")?,
                invocations: n("invocations")?,
                output: n("output")?,
                index_stride: n("index_stride")?,
                stride: n("stride")?,
            })
        });
        Self {
            entry_gs: text("entry_geometry_capture"),
            entry_tf: text("entry_tf"),
            tf_strides,
            geometry,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_facts_of_a_reader_and_of_a_kernel() {
        let j = crate::json::parse(
            r#"{"entry":"main0","entry_tf":"main0_tf","tf_strides":[52,0,0,0],"entry_geometry_capture":"main0_gs"}"#,
        );
        let f = Facts::read(j.as_ref().expect("json"));
        assert_eq!(f.entry_tf.as_deref(), Some("main0_tf"));
        assert_eq!(f.entry_gs.as_deref(), Some("main0_gs"));
        assert_eq!(f.tf_strides, [52, 0, 0, 0]);
        assert_eq!(f.geometry, None);
        let j = crate::json::parse(
            r#"{"geometry":{"vertices":16,"invocations":1,"output":1,"index_stride":32,"stride":49}}"#,
        );
        let f = Facts::read(j.as_ref().expect("json"));
        assert_eq!(
            f.geometry,
            Some(Geometry {
                vertices: 16,
                invocations: 1,
                output: 1,
                index_stride: 32,
                stride: 49
            })
        );
        assert_eq!(f.entry_tf, None);
    }
}
