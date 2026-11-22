//! The pipeline key of the old engine (`pso_skey` of `dm/gfx/nvn/nvn_pso_skey.h`), as the hex of an `R` or `M` row of
//! its `pso_manifest.tsv`.
//!
//! The struct is little-endian and 8-byte aligned:
//!
//! | bytes | field |
//! |---|---|
//! | 0..24, 24..48 | the vertex (or mesh) and fragment shader keys, `<key>_b` and zeros |
//! | 48, 49, 50, 51 | the two shader stages, `merged` (a pass the engine merged) and `fz` (a tile-specialized fragment) |
//! | 52..88 | the tile description and padding |
//! | 88.. | `pso_key`: two pointers (zero), `cfmt[8]` (`MTLPixelFormat` values), depth and stencil format, sample count, blend enable bits, channel masks (4 bits a slot), alpha to coverage, `blend[8]` (a 64-bit word each), `attr[16]` (NVN `va_st` words), `stream[16]` (stride and divisor), `lmap[8]`, a pointer, the alpha test and logic flags, `attr_hi[16]`, the border flags |
//!
//! Rows written before later fields were added are shorter: 864, 880, 1008 or 1024 hex digits. A missing field is zero.

/// The bytes of the full key.
const SIZE: usize = 512;
/// The lengths in hex digits of the forms the engine reads.
const LENGTHS: [usize; 4] = [864, 880, 1008, 1024];
const K: usize = 88;

/// One colour target slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    /// The `MTLPixelFormat` value, 0 for no target.
    pub format: u32,
    /// The channel mask.
    pub mask: u8,
    /// Blending is on.
    pub blend_on: bool,
    /// The NVN blend factors and equations: src RGB, dst RGB, src alpha, dst alpha, RGB equation, alpha equation.
    pub blend: [u32; 6],
}

/// A vertex attribute (`va_st`): NVN format, offset, stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attr {
    /// The attribute index.
    pub index: u32,
    /// The NVN vertex format.
    pub format: u32,
    /// The offset in the vertex.
    pub offset: u32,
    /// The stream.
    pub stream: u32,
}

/// What the import uses of a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skey {
    /// The vertex or mesh shader key (16 hex digits), empty for none.
    pub vs: String,
    /// The fragment shader key, empty for none.
    pub fs: String,
    /// The pass was merged by the engine: the slots are physical, not the game's.
    pub merged: bool,
    /// The colour targets.
    pub slots: [Slot; 8],
    /// The sample count.
    pub samples: u32,
    /// Alpha to coverage.
    pub a2c: bool,
    /// The vertex attributes with a format, by index.
    pub attrs: Vec<Attr>,
    /// The strides and divisors of the 16 streams.
    pub streams: [(u32, u32); 16],
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .map_or(0, |w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut w = [0u8; 8];
    if let Some(s) = b.get(at..at + 8) {
        w.copy_from_slice(s);
    }
    u64::from_le_bytes(w)
}

/// The shader key of a 24-byte name field (`<key>_b`), empty when the field is empty.
fn name(b: &[u8]) -> Option<String> {
    let end = b.iter().position(|c| *c == 0).unwrap_or(b.len());
    let s = std::str::from_utf8(&b[..end]).ok()?;
    if s.is_empty() {
        return Some(String::new());
    }
    let k = s.strip_suffix("_b")?;
    nxkit_pipestate::seeds::is_key(k).then(|| k.to_string())
}

/// Decodes the hex of a key. None for a length or a digit the engine would refuse, or a shader name that is not a key.
pub fn decode(hex: &str) -> Option<Skey> {
    if !LENGTHS.contains(&hex.len()) {
        return None;
    }
    let mut b = vec![0u8; SIZE];
    for (i, pair) in hex.as_bytes().chunks(2).enumerate() {
        let s = std::str::from_utf8(pair).ok()?;
        b[i] = u8::from_str_radix(s, 16).ok()?;
    }
    let blend_en = u32_at(&b, K + 60);
    let chmask = u32_at(&b, K + 64);
    let mut slots = [Slot {
        format: 0,
        mask: 0,
        blend_on: false,
        blend: [0; 6],
    }; 8];
    for (i, s) in slots.iter_mut().enumerate() {
        let w = u64_at(&b, K + 72 + 8 * i);
        let bits = |at: u32, n: u32| ((w >> at) & ((1 << n) - 1)) as u32;
        *s = Slot {
            format: u32_at(&b, K + 16 + 4 * i),
            mask: ((chmask >> (4 * i)) & 0xF) as u8,
            blend_on: blend_en >> i & 1 == 1,
            blend: [
                bits(3, 7),
                bits(10, 7),
                bits(17, 7),
                bits(24, 7),
                bits(31, 3),
                bits(34, 3),
            ],
        };
    }
    let mut attrs = Vec::new();
    for i in 0..32u32 {
        let at = if i < 16 {
            K + 136 + 4 * i as usize
        } else {
            K + 352 + 4 * (i as usize - 16)
        };
        let w = u32_at(&b, at);
        if w & 0xFF != 0 {
            attrs.push(Attr {
                index: i,
                format: w & 0xFF,
                offset: (w >> 8) & 0x7_FFFF,
                stream: w >> 27,
            });
        }
    }
    let mut streams = [(0, 0); 16];
    for (i, s) in streams.iter_mut().enumerate() {
        *s = (u32_at(&b, K + 200 + 8 * i), u32_at(&b, K + 204 + 8 * i));
    }
    Some(Skey {
        vs: name(&b[0..24])?,
        fs: name(&b[24..48])?,
        merged: b[50] != 0,
        slots,
        samples: u32_at(&b, K + 56),
        a2c: u32_at(&b, K + 68) != 0,
        attrs,
        streams,
    })
}
