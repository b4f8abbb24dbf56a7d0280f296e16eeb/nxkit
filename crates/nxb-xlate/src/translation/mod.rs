//! The state of one translation: header, definitions, host answers, shader properties, resources, attribute usage and
//! the function patterns. The pipeline that runs a translation is `crate::front`.

pub mod context;
pub mod definitions;
pub mod fsi_match;
#[cfg(test)]
mod fsi_match_tests;
pub mod fsi_patterns;
pub mod header;
pub mod host;
pub mod props;
pub mod props_fmt;
pub mod resources;
pub mod resources_bind;
pub mod resources_res;
pub mod vac;
