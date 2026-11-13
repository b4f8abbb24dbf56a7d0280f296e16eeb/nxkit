//! Preserve section addresses across the kernel's contiguous-module ABI.
//!
//! `sys::kernel_init` forwards only a module base and four sizes. The kernel lays
//! those sizes out consecutively, but an NSO can have gaps and the build widens
//! DATA downward to a 16 KiB boundary. Pass one synthetic module per real section
//! instead. These descriptors are ONLY for the kernel call, never the loader.

use crate::module::{Module, Prot, Region};
use crate::{Game, HostError, Result};

const PAGE: u64 = 0x1000;

#[derive(Clone, Copy)]
enum Kind {
    Text,
    ReadOnly,
    Writable,
}

fn bad(module: &Module, message: &str) -> HostError {
    HostError::Memory(format!("module {}: kernel map: {message}", module.name))
}

fn end(module: &Module, start: u64, size: u64) -> Result<u64> {
    start
        .checked_add(size)
        .ok_or_else(|| bad(module, "section range overflows"))
}

fn rounded_end(module: &Module, value: u64) -> Result<u64> {
    value
        .checked_add(PAGE - 1)
        .map(|v| v & !(PAGE - 1))
        .ok_or_else(|| bad(module, "page rounding overflows"))
}

fn append(out: &mut Vec<Module>, module: &Module, start: u64, end: u64, kind: Kind) -> Result<()> {
    if start == end {
        return Ok(());
    }
    if start > end || !start.is_multiple_of(PAGE) {
        return Err(bad(module, "section start is not a 4 KiB page boundary"));
    }
    let end = rounded_end(module, end)?;
    let protection = match kind {
        Kind::Text | Kind::ReadOnly => Prot::Exec,
        Kind::Writable => Prot::Write,
    };
    // Containment in one actual segment prevents a descriptor from spanning an
    // unmapped gap or acquiring permissions its host pages do not have.
    let contained = module.segments.iter().any(|segment| {
        let low = i128::from(segment.offset);
        let high = low + i128::from(segment.size);
        segment.prot == protection && i128::from(start) >= low && i128::from(end) <= high
    });
    if !contained {
        return Err(bad(
            module,
            "section leaves its mapped segment or has incompatible protection",
        ));
    }
    let base = module
        .base
        .checked_add(start)
        .ok_or_else(|| bad(module, "section address overflows"))?;
    module
        .base
        .checked_add(end)
        .ok_or_else(|| bad(module, "section end address overflows"))?;
    if !base.is_multiple_of(PAGE) {
        return Err(bad(module, "section address is not 4 KiB aligned"));
    }
    let size = end - start;
    let mut descriptor = module.clone();
    descriptor.base = base;
    descriptor.text = Region::default();
    descriptor.rodata = Region::default();
    descriptor.data = Region::default();
    descriptor.bss_size = 0;
    match kind {
        Kind::Text => descriptor.text.size = size,
        Kind::ReadOnly => descriptor.rodata.size = size,
        Kind::Writable => descriptor.data.size = size,
    }
    out.push(descriptor);
    Ok(())
}

/// Make a temporary configuration for `sys::kernel_init`, preserving `game`.
///
/// The returned modules are ABI descriptors, not loadable modules. Only their
/// base, zero text offset, and text/rodata/data/bss sizes are meaningful. Do not
/// pass this clone to placement, loading, symbol resolution, or `Game::check`.
pub(crate) fn configuration(game: &Game) -> Result<Game> {
    let mut modules = Vec::new();
    for module in &game.modules {
        let text_end = end(module, module.text.offset, module.text.size)?;
        let ro_end = end(module, module.rodata.offset, module.rodata.size)?;
        let data_end = end(module, module.data.offset, module.data.size)?;
        let writable_end = end(module, data_end, module.bss_size)?;
        let has_writable = writable_end > module.data.offset;
        if module.text.size > 0 {
            let text_page_end = rounded_end(module, text_end)?;
            if (module.rodata.size > 0 && text_page_end > module.rodata.offset)
                || (has_writable && text_page_end > module.data.offset)
            {
                return Err(bad(module, "executable text overlaps another section"));
            }
            append(
                &mut modules,
                module,
                module.text.offset,
                text_end,
                Kind::Text,
            )?;
        }
        if module.rodata.size > 0 {
            if !module.rodata.offset.is_multiple_of(PAGE) {
                return Err(bad(module, "read-only section is not 4 KiB aligned"));
            }
            if has_writable && ro_end > module.data.offset && ro_end > writable_end {
                return Err(bad(
                    module,
                    "read-only overlap extends beyond the writable section",
                ));
            }
            // The build's DATA segment begins at floor(original DATA_OFF, 16 KiB).
            // Its writable prefix can therefore include the tail of original RO.
            // Give DATA that overlap explicitly instead of depending on map order.
            let clipped = if has_writable {
                ro_end.min(module.data.offset)
            } else {
                ro_end
            };
            if clipped > module.rodata.offset {
                append(
                    &mut modules,
                    module,
                    module.rodata.offset,
                    clipped,
                    Kind::ReadOnly,
                )?;
            }
        }
        if has_writable {
            // DATA and BSS share their boundary page. Rounding both sizes would
            // incorrectly claim one extra page when neither size is page aligned.
            append(
                &mut modules,
                module,
                module.data.offset,
                writable_end,
                Kind::Writable,
            )?;
        }
    }
    let mut config = game.clone();
    config.modules = modules;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::{Patch, Segment};
    use nxkit_hos::{Config, FakeMemory, GuestMemory, Kernel, MainThread, MemoryConfig, Thread};
    use std::{path::Path, sync::Arc};

    const BASE: u64 = 0x10_0000;
    const MODULE: u64 = BASE + 0x4_0000;
    const SCRATCH: u64 = BASE + 0x3_0000;
    const SIZE: u64 = 0x80_0000;

    fn game(text: (u64, u64), ro: (u64, u64), data: (u64, u64), bss: u64) -> Game {
        let mut game = Game::parse(
            Path::new("/test"),
            &crate::manifest::sample("", crate::module::samples::NEAR),
        )
        .unwrap();
        let m = &mut game.modules[0];
        m.base = MODULE;
        m.text = Region {
            offset: text.0,
            size: text.1,
        };
        m.rodata = Region {
            offset: ro.0,
            size: ro.1,
        };
        m.data = Region {
            offset: data.0,
            size: data.1,
        };
        m.bss_size = bss;
        m.key_page = (data.0 + data.1 + bss).next_multiple_of(0x4000);
        m.size = m.key_page + 0x4000;
        m.low = 0;
        m.span = m.size;
        m.patch = Patch::default();
        m.segments = vec![
            Segment {
                name: "TEXT".into(),
                offset: 0,
                size: data.0,
                file_offset: 0,
                file_size: data.0,
                prot: Prot::Exec,
            },
            Segment {
                name: "DATA".into(),
                offset: data.0 as i64,
                size: m.size - data.0,
                file_offset: data.0,
                file_size: data.1,
                prot: Prot::Write,
            },
        ];
        m.check().unwrap();
        game
    }

    fn sparse() -> Game {
        game((0, 0x3000), (0x8000, 0x6000), (0x10000, 0x1800), 0x1000)
    }

    fn records(game: &Game) -> Vec<(u64, u64, u64, u64, u64, u64)> {
        game.modules
            .iter()
            .map(|m| {
                (
                    m.base,
                    m.text.offset,
                    m.text.size,
                    m.rodata.size,
                    m.data.size,
                    m.bss_size,
                )
            })
            .collect()
    }

    #[test]
    fn sparse_sections_keep_their_addresses_and_input_is_unchanged() {
        let game = sparse();
        let before = format!("{game:?}");
        assert_eq!(
            records(&configuration(&game).unwrap()),
            vec![
                (MODULE, 0, 0x3000, 0, 0, 0),
                (MODULE + 0x8000, 0, 0, 0x6000, 0, 0),
                (MODULE + 0x10000, 0, 0, 0, 0x3000, 0),
            ]
        );
        assert_eq!(format!("{game:?}"), before);
    }

    #[test]
    fn widened_data_wins_the_original_read_only_overlap() {
        let game = game((0, 0x4000), (0x4000, 0x7000), (0x8000, 0x4000), 0x1000);
        assert_eq!(
            records(&configuration(&game).unwrap()),
            vec![
                (MODULE, 0, 0x4000, 0, 0, 0),
                (MODULE + 0x4000, 0, 0, 0x4000, 0, 0),
                (MODULE + 0x8000, 0, 0, 0, 0x5000, 0),
            ]
        );
    }

    #[test]
    fn data_and_bss_round_once() {
        let game = game((0, 0x4000), (0x4000, 0x4000), (0x8000, 0x1800), 0x800);
        assert_eq!(
            configuration(&game)
                .unwrap()
                .modules
                .last()
                .unwrap()
                .data
                .size,
            0x2000
        );
    }

    #[test]
    fn rejects_overflow_unaligned_or_unmapped_sections() {
        let mut g = sparse();
        g.modules[0].data.size = u64::MAX;
        assert!(configuration(&g).is_err());
        let mut g = sparse();
        g.modules[0].base = u64::MAX - 0x1fff;
        assert!(configuration(&g).is_err());
        let mut g = sparse();
        g.modules[0].rodata.offset += 1;
        assert!(configuration(&g).is_err());
        let mut g = sparse();
        g.modules[0].segments[0].size = 0x9000;
        assert!(configuration(&g).is_err());
        let mut g = sparse();
        g.modules[0].segments[1].prot = Prot::Exec;
        assert!(configuration(&g).is_err());
        let mut g = sparse();
        g.modules[0].rodata.offset = 0x2000;
        assert!(configuration(&g).is_err());
        let mut g = sparse();
        g.modules[0].rodata.offset = 0x2_0000;
        assert!(configuration(&g).is_err());
    }

    struct Rig {
        kernel: Arc<Kernel>,
        mem: Arc<FakeMemory>,
        main: Arc<Thread>,
    }
    impl Rig {
        fn new(game: &Game) -> Self {
            let mem = Arc::new(FakeMemory::new(BASE, (SIZE / 4) as usize));
            // This is exactly the unchanged sys::kernel_init -> NxkitModule
            // projection, exercised against the real portable kernel, not a model.
            let modules = game
                .modules
                .iter()
                .map(|m| nxkit_hos::Module {
                    base: m.base + m.text.offset,
                    text: m.text.size,
                    rodata: m.rodata.size,
                    data: m.data.size,
                    bss: m.bss_size,
                })
                .collect();
            let config = Config {
                memory: Some(MemoryConfig {
                    host: mem.clone(),
                    base: BASE,
                    size: SIZE,
                    front_size: 0x10_0000,
                    stack_size: 0x10_0000,
                    alias_size: 0x10_0000,
                    app_pool: 0x40_0000,
                    used_memory: 0x10_0000,
                    system_resource: 0,
                    map_limit: false,
                    program_id: 1,
                    tls: (BASE, 0x1_0000),
                    shared: (BASE + 0x2_0000, 0x1_0000),
                    main_stack: None,
                    modules,
                }),
                main_thread: Some(MainThread {
                    priority: 28,
                    core: 1,
                    stack_size: 0x10000,
                }),
                apply_qos: false,
                log: |_| {},
                ..Config::default()
            };
            let kernel = Kernel::try_new(config, mem.clone()).unwrap();
            let main = kernel.attach_main_thread("kernel-map-test").unwrap();
            Self { kernel, mem, main }
        }
        fn svc(&self, number: u32, args: &[u64]) -> [u64; 8] {
            let mut regs = [0; 8];
            regs[..args.len()].copy_from_slice(args);
            assert!(self.kernel.dispatch_svc(&self.main, number, &mut regs));
            regs
        }
        fn query(&self, offset: u64) -> (u64, u64, u32, u32) {
            assert_eq!(self.svc(0x06, &[SCRATCH, 0, MODULE + offset])[0], 0);
            (
                self.mem.load64(SCRATCH).unwrap(),
                self.mem.load64(SCRATCH + 8).unwrap(),
                self.mem.load32(SCRATCH + 16).unwrap(),
                self.mem.load32(SCRATCH + 24).unwrap(),
            )
        }
    }

    #[test]
    fn actual_kernel_queries_preserve_sparse_sections_and_reject_the_old_projection() {
        let game = sparse();
        let old = Rig::new(&game);
        assert_eq!(old.query(0x10000).2, 0x10);
        assert_ne!(old.svc(0x02, &[MODULE + 0x10000, PAGE, 3])[0], 0);
        let fixed = Rig::new(&configuration(&game).unwrap());
        assert_eq!(fixed.query(0x1000), (MODULE, 0x3000, 3, 5));
        assert_eq!(fixed.query(0x8000), (MODULE + 0x8000, 0x6000, 3, 1));
        assert_eq!(fixed.query(0x11000), (MODULE + 0x10000, 0x3000, 4, 3));
        for hole in [0x3000, 0x7000, 0xe000, 0xf000, 0x13000] {
            let info = fixed.query(hole);
            assert_eq!((info.2, info.3), (0x10, 0));
        }
        assert_eq!(fixed.svc(0x02, &[MODULE + 0x10000, PAGE, 3])[0], 0);
    }

    #[test]
    fn actual_kernel_queries_use_writable_overlap_without_an_extra_bss_page() {
        let game = game((0, 0x4000), (0x4000, 0x7000), (0x8000, 0x4800), 0x800);
        let fixed = Rig::new(&configuration(&game).unwrap());
        assert_eq!(fixed.query(0x4000), (MODULE + 0x4000, 0x4000, 3, 1));
        assert_eq!(fixed.query(0x8000), (MODULE + 0x8000, 0x5000, 4, 3));
        assert_eq!((fixed.query(0xd000).2, fixed.query(0xd000).3), (0x10, 0));
    }
}
