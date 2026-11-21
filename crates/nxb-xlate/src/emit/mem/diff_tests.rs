//! Debug test: `NXB_EM_DIFF=<InstName> [NXB_EM_N=3] cargo test -p nxb-xlate em_diff -- --nocapture` prints differing chunks.

use crate::front::pipeline::{chunks, emit_text_lenient};

#[test]
fn em_diff() {
    let Ok(name) = std::env::var("NXB_EM_DIFF") else {
        return;
    };
    let max: usize = std::env::var("NXB_EM_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let Some(entries) = crate::corpus::load() else {
        return;
    };
    let mut shown = 0;
    for e in &entries {
        let Some(want) = e.first_attempt("emit") else {
            continue;
        };
        let Ok((got, errs)) = emit_text_lenient(&e.job, &e.code) else {
            continue;
        };
        let (g, w) = (chunks(&got), chunks(&want));
        if g.len() != w.len() {
            continue;
        }
        for (a, b) in g.iter().zip(w.iter()) {
            if b.0 == name && a.1 != b.1 {
                println!("---- ours ----\n{}---- want ----\n{}", a.1, b.1);
                for er in errs.iter().filter(|x| x.0 == name).take(1) {
                    println!("error: {}", er.1);
                }
                if shown == 0 {
                    let _ = std::fs::write(std::env::temp_dir().join("em-ours.txt"), &got);
                    let _ = std::fs::write(std::env::temp_dir().join("em-want.txt"), &want);
                }
                shown += 1;
                if shown >= max {
                    return;
                }
            }
        }
    }
}
