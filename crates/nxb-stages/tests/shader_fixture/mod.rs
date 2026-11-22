use nxb_tool::{FrontEnd, airfront::RawFrontEnd};
use nxb_xlate::{
    job::Job,
    msl::{Context, translate},
};

pub(super) fn check_shader(job: &Job, code: &[u8]) -> Result<String, String> {
    let msl = translate(job, code, &Context::default())
        .map_err(|e| format!("translation failed: {e:?}"))?
        .libraries
        .into_iter()
        .find(|lib| lib.suffix == "_b")
        .ok_or("missing base library")?
        .msl;
    for ios in [false, true] {
        RawFrontEnd::new(ios)
            .compile(msl.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    Ok(msl)
}
