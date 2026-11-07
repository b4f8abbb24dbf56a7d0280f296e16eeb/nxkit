use super::*;

fn build(dir: &str) -> Build {
    let w = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/layer-tests")
        .join(format!("{dir}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&w);
    std::fs::create_dir_all(&w).unwrap();
    let mut b = Build::for_test(w.clone(), w.join("profile"));
    b.shaders = w.join("shader/cache/b-1");
    b
}

#[test]
fn without_an_extract_the_profile_names_the_api() {
    let mut b = build("profile");
    assert_eq!(gfx(&b).unwrap(), Gfx::Nvn);
    b.profile
        .1
        .insert("graphics".into(), "vulkan the reason".into());
    assert_eq!(gfx(&b).unwrap(), Gfx::Vulkan);
    // the profile's answer is not kept: the extract decides once it is there
    assert!(b.gfx.get().is_none());
    assert_eq!(msl_dir(&b).unwrap(), b.work.join("shader/vk"));
    b.profile.1.insert("graphics".into(), "gl".into());
    assert_eq!(msl_dir(&b).unwrap(), b.work.join("shader/gl"));
    let _ = std::fs::remove_dir_all(&b.work);
}

#[test]
fn a_found_api_is_kept_and_picks_the_library_folder() {
    let b = build("kept");
    b.gfx.set((Gfx::Gl, "found".into())).unwrap();
    assert_eq!(gfx_why(&b).unwrap(), (Gfx::Gl, "found".to_string()));
    assert_eq!(msl_dir(&b).unwrap(), b.work.join("shader/gl"));
    let nvn = build("nvn");
    nvn.gfx.set((Gfx::Nvn, String::new())).unwrap();
    assert_eq!(msl_dir(&nvn).unwrap(), nvn.shaders);
    let _ = std::fs::remove_dir_all(&b.work);
    let _ = std::fs::remove_dir_all(&nvn.work);
}

#[test]
fn a_shaders_folder_named_for_the_api_is_taken() {
    let mut b = build("given");
    b.shaders = PathBuf::from("/some/where/vk");
    b.gfx.set((Gfx::Vulkan, String::new())).unwrap();
    assert_eq!(msl_dir(&b).unwrap(), PathBuf::from("/some/where/vk"));
    let _ = std::fs::remove_dir_all(&b.work);
}

#[test]
fn par_map_keeps_the_order() {
    let v: Vec<u32> = (0..100).collect();
    assert_eq!(
        par_map(&v, 7, &crate::events::Count::default(), |x| x * 2),
        v.iter().map(|x| x * 2).collect::<Vec<_>>()
    );
    assert!(par_map(&Vec::<u32>::new(), 4, &crate::events::Count::default(), |x| *x).is_empty());
}

#[test]
fn the_dlc_images_follow_the_packed_romfs() {
    let b = build("images");
    let aoc = b.work.join("aoc");
    std::fs::create_dir_all(aoc.join("2")).unwrap();
    std::fs::write(aoc.join("1.img"), b"").unwrap();
    std::fs::write(aoc.join("notes.txt"), b"").unwrap();
    assert_eq!(
        images(&b),
        vec![
            b.work.join("extract/v0/RomFS.img"),
            aoc.join("1.img"),
            aoc.join("2")
        ]
    );
    let _ = std::fs::remove_dir_all(&b.work);
}

#[test]
fn the_capture_folders_follow_the_romfs() {
    let b = build("captures");
    let ex = b.work.join("extract/v0");
    std::fs::create_dir_all(ex.join("gl_capture")).unwrap();
    std::fs::create_dir_all(b.work.join("aoc/2")).unwrap();
    assert_eq!(
        images(&b),
        vec![
            ex.join("RomFS.img"),
            ex.join("gl_capture"),
            b.work.join("aoc/2")
        ]
    );
    std::fs::create_dir_all(ex.join("vk_capture")).unwrap();
    assert_eq!(images(&b)[1], ex.join("vk_capture"));
    let _ = std::fs::remove_dir_all(&b.work);
}

#[test]
fn msl_files_are_written_once_per_key_and_stage() {
    let b = build("msl");
    let s = |key: &str, stage, dual: bool| Shader {
        key: key.into(),
        stage,
        fields: String::new(),
        msl: format!("// {key} {stage}"),
        msl_dual: dual.then(|| format!("// {key} dual")),
    };
    let list = vec![
        (s("a", "frag", true), "/x".to_string()),
        (s("a", "frag", false), "/y".to_string()),
        (s("b", "vert", false), "/z".to_string()),
    ];
    let (files, n) = write_msl(&list, &b.work).unwrap();
    assert_eq!(n, 2);
    let names: Vec<String> = files
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["a_frag.metal", "a_frag_d.metal", "b_vert.metal"]);
    assert_eq!(std::fs::read_to_string(&files[1]).unwrap(), "// a dual");
    let _ = std::fs::remove_dir_all(&b.work);
}
