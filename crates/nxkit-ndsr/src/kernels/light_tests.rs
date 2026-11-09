use super::*;
use crate::msl::SOURCE;
use crate::net::NETS;
use crate::plan::GameResource;

/// The numbers of a `NDSR_LIGHT(...)` line of the MSL, name first.
fn instantiations() -> Vec<(String, Vec<u64>)> {
    SOURCE
        .lines()
        .filter_map(|l| l.strip_prefix("NDSR_LIGHT(\"")?.strip_suffix(')'))
        .map(|l| {
            let (name, rest) = l.split_once("\", ").expect("name");
            let nums = rest
                .split(", ")
                .map(|v| match v {
                    "true" => 1,
                    "false" => 0,
                    _ => v.parse().expect("number"),
                })
                .collect();
            (name.to_string(), nums)
        })
        .collect()
}

/// The Rust table and the MSL instantiations are the same 15 programs.
#[test]
fn table_matches_the_msl_instantiations() {
    let inst = instantiations();
    assert_eq!(inst.len(), LIGHT_KERNELS.len());
    for (k, (name, n)) in LIGHT_KERNELS.iter().zip(&inst) {
        assert_eq!(&k.name, name);
        let want = [
            k.in_px,
            k.out_px,
            k.step[0],
            k.step[1],
            k.layers,
            u32::from(k.f32_luma),
            u32::from(k.srgb),
            k.simdgroups,
            k.patch_group,
        ]
        .map(u64::from);
        assert_eq!(n, &want, "{name}");
        assert!(k.threadgroup_bytes() <= 32768, "{name}");
        assert!((1..=16).contains(&k.simdgroups) && (1..=4).contains(&k.patch_group));
    }
}

/// Every program of the pack has its kernel, and each kernel serves one program (two entries of the pack may share
/// it); the other packs have none.
#[test]
fn every_light_program_has_a_kernel() {
    let mut used = vec![0; LIGHT_KERNELS.len()];
    for n in &NETS {
        match find(n) {
            Some(k) => {
                assert_eq!(n.pack, Pack::VrLight, "{}", n.key);
                assert_eq!(k.layers as usize, n.blob().layers(), "{}", n.key);
                let i = LIGHT_KERNELS.iter().position(|o| o == k).expect("in table");
                used[i] += 1;
            }
            None => assert_ne!(n.pack, Pack::VrLight, "{}", n.key),
        }
    }
    assert!(used.iter().all(|&u| u == 1), "{used:?}");
    let ocarina = Net::by_key("fa3033d05c0a41d6").expect("net").1;
    assert_eq!(find(ocarina).map(|k| k.name), Some("ndsr_light_r43_b"));
}

fn push(tile: [u32; 2]) -> Vec<u8> {
    let mut w = [0u32; 29];
    w[18] = tile[0];
    w[19] = tile[1];
    w.iter().flat_map(|v| v.to_le_bytes()).collect()
}

#[test]
fn plan_shape() {
    let net = Net::by_key("fa3033d05c0a41d6").expect("net").1;
    let p = plan(net, &[0; 0x1800], [1536, 864], &push([64, 32])).expect("plan");
    assert_eq!(p.buffers[0].bytes, 0x1800);
    let [s] = &p.steps[..] else {
        panic!("one step")
    };
    // the game's dispatch of Ocarina of Time: 32 x 36 threadgroups
    assert_eq!(s.groups, [32, 36, 1]);
    assert_eq!(s.kernel, "ndsr_light_r43_b");
    assert_eq!(s.textures[1].source, Source::Game(GameResource::Output));
    assert!(s.textures[1].writes && !s.textures[0].writes);
}

#[test]
fn plan_errors() {
    let net = Net::by_key("fa3033d05c0a41d6").expect("net").1;
    let ok = push([64, 32]);
    assert!(plan(net, &[0; 0x3000], [1536, 864], &ok).is_err());
    assert!(plan(net, &[0; 0x1800], [0, 864], &ok).is_err());
    assert!(plan(net, &[0; 0x1800], [1536, 864], &ok[..80]).is_err());
    assert!(plan(net, &[0; 0x1800], [1536, 864], &ok[..112]).is_err());
    let e = plan(net, &[0; 0x1800], [1536, 864], &push([48, 32])).unwrap_err();
    assert!(e.contains("tile"), "{e}");
    let vr = Net::by_key("14c10aa70b81f04e").expect("net").1;
    assert!(plan(vr, &[0; 0x3000], [64, 64], &ok).is_err());
}
