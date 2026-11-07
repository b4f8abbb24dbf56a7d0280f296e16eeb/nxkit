//! The node list of one build run level by level ([`crate::order`]), for a Mac app, a Mac-only build and the program
//! sub-builds of a multi-program application.

use crate::Args;
use crate::events::Progress;
use crate::graph::Build;
use crate::order::{Stage, Work};
use std::sync::Mutex;

/// Runs the nodes of `b`. Stages overlap where the graph allows: the Mac host from the start, the app's companion app,
/// icon and settings once the extract and the host are done (beside the shader step), the first archive streamed
/// beside the shader step ([`crate::pipeline`]). `programs`: the other programs of a multi-program application, packed
/// into the Mac app.
pub(crate) fn run_nodes(
    a: &Args,
    b: &Build,
    p: &std::sync::Arc<Progress>,
    programs: &[nxb_pack::macapp::Program],
) -> Result<(), String> {
    p.plan_more(b.nodes.len());
    let streamed: Mutex<Option<String>> = Mutex::new(None);
    let archive_error: Mutex<Option<String>> = Mutex::new(None);
    let no_archive = std::sync::atomic::AtomicBool::new(false);
    // the script's rule: a pipeline archive that fails is no archive for the app (the host builds its bases on the first
    // start), the build goes on
    let give_up = |e: String| -> crate::order::Outcome {
        p.say(&format!(
            "[RED] pipeline archive: {e}; the app is packed without one and builds its pipeline bases on its first start (--gpu none skips this step)"
        ));
        no_archive.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok("failed, the app is packed without one".into())
    };
    let (archive_error, no_archive) = (&archive_error, &no_archive);
    let parts: Mutex<Option<crate::host::app::AppParts>> = Mutex::new(None);
    let (streamed, parts) = (&streamed, &parts);
    let previous = b.out.is_dir().then(|| b.out.clone());
    let host = b.nodes.iter().position(|n| n.name == "Mac host");
    let mut stages: Vec<Stage<'_>> = Vec::new();
    for (i, n) in b.nodes.iter().enumerate() {
        let mut deps = n.deps.clone();
        let work = match (&n.reused, i, n.name.as_str()) {
            // reused images still need their patched modules: the rewrite runs (or stops the build) here
            (Some(r), 0, _) => {
                let r = r.clone();
                Work::Turn(Box::new(move || {
                    let note =
                        crate::rewrite::ensure_with(&b.engine, &b.profile.0, &b.work, &b.package, Some(p))?;
                    crate::boot::run::after_rewrite(a, b, p)?;
                    Ok(format!(
                        "reused: {r}{}",
                        note.map(|n| format!(", {n}")).unwrap_or_default()
                    ))
                }))
            }
            (Some(r), _, _) => Work::Reused(format!("reused: {r}")),
            (_, 0, _) => Work::Turn(Box::new(move || crate::extract_stage::run(a, b, p))),
            (_, 1, _) if b.split_fused() => Work::Reused("in the extract's pass".into()),
            (_, 1, _) => Work::Turn(Box::new(move || {
                if crate::layer::gfx(b)? != crate::layer::Gfx::Nvn {
                    return Ok("not needed: the game does not draw with NVN".into());
                }
                crate::extract_stage::split(a, b, p)
            })),
            (_, 2, _) => Work::Turn(Box::new(move || {
                if crate::layer::gfx(b)? != crate::layer::Gfx::Nvn {
                    return crate::layer::stage(a, b, p);
                }
                crate::provisional::take_tables(b)?;
                // the shader step and the first architecture's archive together (crate::pipeline)
                let o = crate::pipeline::shaders_and_archive(a, b, p)?;
                *streamed.lock().map_err(|_| "lock")? = o.archive;
                *archive_error.lock().map_err(|_| "lock")? = o.archive_error;
                Ok(o.shaders)
            })),
            (_, _, name) if name.starts_with("Pipeline archive") => {
                let arch = name.trim_start_matches("Pipeline archive ");
                Work::Turn(Box::new(move || {
                    if crate::layer::gfx(b)? != crate::layer::Gfx::Nvn {
                        no_archive.store(true, std::sync::atomic::Ordering::SeqCst);
                        return Ok("not needed: the game does not draw with NVN".into());
                    }
                    let made = streamed.lock().map_err(|_| "lock")?.take();
                    let failed = archive_error.lock().map_err(|_| "lock")?.take();
                    match (made, failed) {
                        (Some(s), _) => Ok(s),
                        (None, Some(e)) => give_up(e),
                        (None, None) => match crate::pipeline::stage_pso(b, arch, p) {
                            Ok(s) => Ok(s),
                            Err(e) => give_up(crate::pipeline::archive_failure(&e)),
                        },
                    }
                }))
            }
            (_, _, "Mac host") => Work::Early(Box::new(move || {
                crate::host::machost::stage(&b.engine, &b.work, &b.title)
            })),
            _ => {
                // the app's parts (companion app, icon, settings) need the extract and the host only: made ahead
                let prev = previous.clone();
                stages.push(Stage {
                    name: None,
                    deps: std::iter::once(0).chain(host).collect(),
                    work: Work::Early(Box::new(move || {
                        let made = crate::host::app::prepare(a, b, prev.as_deref())?;
                        *parts.lock().map_err(|_| "lock")? = Some(made);
                        Ok(String::new())
                    })),
                });
                deps.push(stages.len() - 1);
                let prev = previous.clone();
                Work::Turn(Box::new(move || {
                    let made = parts.lock().map_err(|_| "lock")?.take();
                    crate::app_stage::stage_app(
                        a,
                        b,
                        p,
                        prev,
                        made.ok_or("the app's parts were not made")?,
                        no_archive.load(std::sync::atomic::Ordering::SeqCst),
                        programs,
                    )
                }))
            }
        };
        stages.push(Stage {
            name: Some(n.name.clone()),
            deps,
            work,
        });
    }
    crate::order::run(p, stages)?;
    Ok(())
}
