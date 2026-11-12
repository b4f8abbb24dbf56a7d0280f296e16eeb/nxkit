//! Portable converter/host contract check. Does not execute guest code or trust signatures.
use nxkit_host::{HostError, Result, bundle, image, layout, macho, tail};
use std::path::Path;
fn validate(app: &Path, data: &Path) -> Result<()> {
    let game = bundle::read(app, data)?;
    let path = game.path(&game.dylib);
    let dylib = std::fs::read(&path).map_err(|e| HostError::io(&path, &e))?;
    image::validate(&dylib)?;
    let segments = macho::segments(&dylib)?;
    for module in &game.modules {
        macho::verify(module, &segments)?;
    }
    // nxkit's preferred base is relocatable and can exceed 39 bits. Only dyld
    // can establish the actual slide; place.rs checks that real mapped layout.
    // Here verify the required span, not a runtime address we cannot observe.
    if tail::plan(&game, &segments, 0)?.layout.all.len >= layout::SPACE_END {
        return Err(HostError::Memory(
            "Guest span cannot fit the 39-bit address space".into(),
        ));
    }
    // Exercise the actual converter reservation against the host layout for
    // every possible 16 KiB slide phase, including the negative slide from the
    // writer's high preferred base. This is arithmetic, not a dyld load test.
    let lowest = game
        .modules
        .iter()
        .map(|m| m.start())
        .min()
        .ok_or_else(|| HostError::Memory("Guest has no modules".into()))?;
    let baseline = 0x1_0000_0000i64
        .checked_sub(i64::try_from(lowest).map_err(|_| {
            HostError::Memory("Preferred guest address does not fit a slide".into())
        })?)
        .ok_or_else(|| HostError::Memory("Guest slide overflow".into()))?;
    for phase in (0..layout::REGION_ALIGN).step_by(layout::PAGE as usize) {
        tail::plan(&game, &segments, baseline + phase as i64)?;
    }
    println!(
        "Native bundle contract accepted: title {}, {} modules; all 128 tail slide phases checked. Actual dyld placement, signing trust, execution and gameplay remain unverified.",
        game.title_id,
        game.modules.len()
    );
    Ok(())
}
fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 2 {
        eprintln!("Usage: validate_bundle /absolute/path/Game.app");
        std::process::exit(2);
    }
    let data = std::env::temp_dir().join(format!(
        "nx2ipa-contract-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let result = validate(Path::new(&args[1]), &data);
    if data.exists() {
        if let Err(e) = std::fs::remove_dir_all(&data) {
            eprintln!("Cannot remove test data {}: {e}", data.display());
        }
    }
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
