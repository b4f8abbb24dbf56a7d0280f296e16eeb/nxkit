//! Tests for the folder rules of the runtime cache and for the loader with nothing recorded.

use super::*;

fn files(sizes: &[u64]) -> Vec<(String, u64)> {
    sizes
        .iter()
        .enumerate()
        .map(|(i, s)| (format!("rt-{i:012}-0000.mtl4archive"), *s))
        .collect()
}

#[test]
fn the_oldest_archives_go_first_when_the_folder_is_over_its_size() {
    let f = files(&[100, 100, 100, 100]);
    assert!(prune(&f, 400, 64).is_empty());
    assert_eq!(prune(&f, 250, 64), vec![f[0].0.clone(), f[1].0.clone()]);
    assert_eq!(prune(&f, 0, 64).len(), 4);
}

#[test]
fn too_many_files_are_cut_to_the_newest() {
    let f = files(&[1; 10]);
    assert_eq!(
        prune(&f, 1 << 30, 7),
        vec![f[0].0.clone(), f[1].0.clone(), f[2].0.clone()]
    );
}

#[test]
fn the_identity_tells_every_part_apart() {
    let a = identity(&["1", "23"]);
    assert_eq!(a, identity(&["1", "23"]));
    assert_ne!(a, identity(&["12", "3"]));
    assert_ne!(a, identity(&["1", "24"]));
    assert_ne!(a, identity(&["1", "23", ""]));
}

#[test]
fn the_archive_names_sort_by_time_then_number() {
    let (a, b) = (archive_name(2), archive_name(10));
    assert!(a.starts_with(ARCHIVE_PREFIX) && a.ends_with(".mtl4archive"));
    assert!(a < b);
}

#[test]
fn only_the_archives_of_the_cache_are_listed() {
    let d = std::env::temp_dir().join(format!("nxkit-runtime-list-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    for (n, body) in [
        ("rt-000000000002-0000.mtl4archive", "bb"),
        ("rt-000000000001-0000.mtl4archive", "a"),
        ("rt-000000000003-0000.mtl4archive.partial", "x"),
        ("pipelines.seen", "y"),
        ("other.mtl4archive", "z"),
    ] {
        std::fs::write(d.join(n), body).unwrap();
    }
    let l = archives(&d);
    assert_eq!(
        l,
        vec![
            ("rt-000000000001-0000.mtl4archive".to_string(), 1),
            ("rt-000000000002-0000.mtl4archive".to_string(), 2)
        ]
    );
    assert!(archives(&d.join("missing")).is_empty());
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn an_atomic_write_leaves_no_temporary_file() {
    let d = std::env::temp_dir().join(format!("nxkit-runtime-write-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("a.list");
    write_atomic(&p, b"one").unwrap();
    write_atomic(&p, b"two").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"two");
    assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1);
    std::fs::remove_dir_all(&d).unwrap();
}

/// With no list, or a list of another version, the loader has nothing to make and the cache behaves as before.
#[test]
fn a_missing_or_foreign_list_gives_an_empty_one() {
    assert!(Seen::parse("").is_err());
    assert_eq!(Seen::parse(nxkit_pipestate::list::HEADER).unwrap().len(), 0);
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let p = Pipelines::new(&ctx, &ArchiveConfig::default()).unwrap();
    let before = p.counts();
    p.prewarm(
        &Seen::default(),
        0,
        Path::new("/nonexistent"),
        None,
        None,
        None,
    );
    assert_eq!(p.counts(), before);
}

/// A list whose libraries are not in the shader folder is skipped, not an error.
#[test]
fn a_list_of_missing_libraries_is_skipped() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let text = format!(
        "{}\nC\t0000000000000001\tgone_b\tmain0\n",
        nxkit_pipestate::list::HEADER
    );
    let seen = Seen::parse(&text).unwrap();
    assert_eq!(seen.len(), 1);
    let p = Pipelines::new(&ctx, &ArchiveConfig::default()).unwrap();
    let before = p.counts();
    p.prewarm(&seen, 0, Path::new("/nonexistent"), None, None, None);
    assert_eq!(p.counts(), before);
}

/// The hook of the prewarm runs once the work is over and before the marker of a finished start is written, so that
/// what the start made is on disk when the marker says it is all there.
#[test]
fn the_after_hook_runs_before_the_marker() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let d = std::env::temp_dir().join(format!("nxkit-after-hook-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let marker = d.join("prewarm.done");
    let text = format!(
        "{}\nC\t0000000000000001\tgone_b\tmain0\n",
        nxkit_pipestate::list::HEADER
    );
    let seen = Seen::parse(&text).unwrap();
    let p = Pipelines::new(&ctx, &ArchiveConfig::default()).unwrap();
    let (flag, m) = (std::sync::Arc::new(AtomicU32::new(0)), marker.clone());
    let f = flag.clone();
    p.prewarm(
        &seen,
        0,
        Path::new("/nonexistent"),
        Some(marker.clone()),
        Some(Box::new(move || {
            // the marker is not there yet
            f.store(if m.exists() { 2 } else { 1 }, Ordering::SeqCst);
        })),
        None,
    );
    assert_eq!(flag.load(Ordering::SeqCst), 1);
    assert!(marker.is_file());
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn the_shipped_list_gets_the_keys_the_encoder_computes() {
    let d = std::env::temp_dir().join(format!("nxkit-shipped-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let path = d.join("shipped.list");
    let text = format!(
        "{}\nR\t-\t-\t0\tv1_b\tmain0_z\tf1_b\tmain0\t0\tTriangle\t1\t0\tRgba8Unorm:f:-\t-\nC\t-\tc1_b\tmain0\n",
        nxkit_pipestate::list::HEADER
    );
    std::fs::write(&path, text).unwrap();
    let s = read_shipped(&path);
    assert_eq!(s.len(), 2);
    let record::Entry::Render(r) = &s.entries()[0] else {
        panic!("a render entry first");
    };
    let colors = vec![Some(nxkit_metal::ColorTarget::new(
        nxkit_metal::Format::Rgba8Unorm,
    ))];
    // the key a draw of `v1` in the depth mode 0 to 1 computes in `render.rs`
    let want = super::super::keys::render_key(&super::super::keys::RenderKey {
        vertex: "v1:z",
        fragment: Some("f1"),
        targets: &super::super::Targets {
            colors: vec![Some(nxkit_metal::Format::Rgba8Unorm)],
            samples: 1,
        },
        icb: false,
        colors: &colors,
        layout: &nxkit_metal::VertexDescriptor::default(),
        pull: false,
        class: nxkit_metal::TopologyClass::Triangle,
        alpha_to_coverage: false,
        flat: false,
        paint: false,
        paint_dir: false,
    });
    assert_eq!(r.key, Some(want));
    assert_eq!(
        r.vertex_key,
        Some(super::super::keys::point_size_key("v1", "main0_z"))
    );
    assert_eq!(
        s.entries()[1].key(),
        Some(super::super::keys::compute_key("c1"))
    );
    assert!(read_shipped(&d.join("missing.list")).is_empty());
}
