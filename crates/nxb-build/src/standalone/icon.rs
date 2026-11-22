//! The app icon: the game's cover (`icon.jpg` of the control data, 256x256) as `Contents/Resources/AppIcon.icns`.
//! The cover lies on the macOS icon grid (a 824-pixel shape in a 1024-pixel canvas) with the rounded corners of
//! the system's shape, so the Dock and Finder show it as a Mac icon and not in a grey frame. `sips` scales the
//! JPEG and writes the sizes, `iconutil` packs them. This module draws the shape.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The name of the icon in `Contents/Resources` (`CFBundleIconFile`).
pub(crate) const NAME: &str = "AppIcon";

const CANVAS: usize = 1024;
const BODY: usize = 824;
/// The exponent of the superellipse that approximates the continuous corners of the system's icon shape.
const SHAPE: f64 = 5.0;
/// The samples per axis for the soft edge of the shape.
const SAMPLES: usize = 4;
/// The sizes of an iconset: points and scale.
const SIZES: [(usize, usize); 10] = [
    (16, 1),
    (16, 2),
    (32, 1),
    (32, 2),
    (128, 1),
    (128, 2),
    (256, 1),
    (256, 2),
    (512, 1),
    (512, 2),
];

fn run(cmd: &mut Command) -> Result<(), String> {
    let o = cmd.output().map_err(|e| format!("{cmd:?}: {e}"))?;
    if o.status.success() {
        return Ok(());
    }
    Err(format!(
        "{:?}: {}",
        cmd.get_program(),
        String::from_utf8_lossy(&o.stderr).trim()
    ))
}

/// Writes `AppIcon.icns` from the cover `jpg` into `resources`. `scratch` holds the intermediate files.
pub(crate) fn write(jpg: &Path, resources: &Path, scratch: &Path) -> Result<String, String> {
    let io = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let _ = std::fs::remove_dir_all(scratch);
    let set = scratch.join(format!("{NAME}.iconset"));
    std::fs::create_dir_all(&set).map_err(|e| io(&set, e))?;
    let bmp = scratch.join("cover.bmp");
    let side = BODY.to_string();
    run(Command::new("sips")
        .args(["-s", "format", "bmp", "-z", &side, &side])
        .arg(jpg)
        .arg("--out")
        .arg(&bmp))?;
    let cover = Rgba::from_bmp(&std::fs::read(&bmp).map_err(|e| io(&bmp, e))?)?;
    if cover.w != BODY || cover.h != BODY {
        return Err(format!(
            "{}: {}x{} after scaling",
            bmp.display(),
            cover.w,
            cover.h
        ));
    }
    let master = scratch.join("master.png");
    std::fs::write(&master, png(&compose(&cover))).map_err(|e| io(&master, e))?;
    for (pt, scale) in SIZES {
        let px = (pt * scale).to_string();
        let file: PathBuf = match scale {
            1 => set.join(format!("icon_{pt}x{pt}.png")),
            _ => set.join(format!("icon_{pt}x{pt}@{scale}x.png")),
        };
        run(Command::new("sips")
            .args(["-s", "format", "png", "-z", &px, &px])
            .arg(&master)
            .arg("--out")
            .arg(&file))?;
    }
    std::fs::create_dir_all(resources).map_err(|e| io(resources, e))?;
    let icns = resources.join(format!("{NAME}.icns"));
    run(Command::new("iconutil")
        .args(["-c", "icns"])
        .arg(&set)
        .arg("-o")
        .arg(&icns))?;
    let _ = std::fs::remove_dir_all(scratch);
    let n = std::fs::metadata(&icns).map_err(|e| io(&icns, e))?.len();
    Ok(format!(
        "icon: the cover of the game ({} KiB)",
        n.div_ceil(1024)
    ))
}

/// An image of 8-bit RGBA pixels, rows top to bottom.
struct Rgba {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Rgba {
    /// A BMP of 24 or 32 bits per pixel (uncompressed or with bit fields), as `sips` writes it.
    fn from_bmp(b: &[u8]) -> Result<Self, String> {
        let bad = |m: &str| format!("the scaled cover is not a BMP we read: {m}");
        let u16at = |o: usize| b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
        let u32at = |o: usize| {
            b.get(o..o + 4)
                .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        };
        if b.get(..2) != Some(b"BM") {
            return Err(bad("signature"));
        }
        let (Some(data), Some(w), Some(h), Some(bpp), Some(comp)) =
            (u32at(10), u32at(18), u32at(22), u16at(28), u32at(30))
        else {
            return Err(bad("header"));
        };
        let (w, h) = (w as i32, h as i32);
        if w <= 0 || h == 0 {
            return Err(bad("size"));
        }
        let (w, top_down, h) = (w as usize, h < 0, h.unsigned_abs() as usize);
        let masks = match (comp, bpp) {
            (0, 24) => [0xff_0000, 0xff00, 0xff, 0],
            (0, 32) => [0xff_0000, 0xff00, 0xff, 0xff00_0000],
            (3 | 6, 32) => {
                let m = |k: usize| u32at(54 + 4 * k).ok_or_else(|| bad("bit fields"));
                [m(0)?, m(1)?, m(2)?, if comp == 6 { m(3)? } else { 0 }]
            }
            _ => return Err(bad(&format!("{bpp} bits, compression {comp}"))),
        };
        let bytes = usize::from(bpp / 8);
        let stride = (w * bytes).div_ceil(4) * 4;
        let data = data as usize;
        if b.len() < data + stride * h {
            return Err(bad("short pixel data"));
        }
        let channel = |v: u32, m: u32| match m {
            0 => 255,
            m => (((v & m) >> m.trailing_zeros()) * 255 / (m >> m.trailing_zeros())) as u8,
        };
        let mut px = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            let row = if top_down { y } else { h - 1 - y };
            let row = &b[data + row * stride..][..w * bytes];
            for p in row.chunks_exact(bytes) {
                let mut v = [0u8; 4];
                v[..bytes].copy_from_slice(p);
                let v = u32::from_le_bytes(v);
                px.extend(masks.map(|m| channel(v, m)));
            }
        }
        Ok(Self { w, h, px })
    }
}

/// The coverage of pixel `(x, y)` of the body by the icon shape, 0 to 255.
fn coverage(x: usize, y: usize) -> u8 {
    let half = BODY as f64 / 2.0;
    let mut inside = 0;
    for sy in 0..SAMPLES {
        for sx in 0..SAMPLES {
            let fx = (x as f64 + (sx as f64 + 0.5) / SAMPLES as f64 - half).abs() / half;
            let fy = (y as f64 + (sy as f64 + 0.5) / SAMPLES as f64 - half).abs() / half;
            if fx.powf(SHAPE) + fy.powf(SHAPE) <= 1.0 {
                inside += 1;
            }
        }
    }
    (inside * 255 / (SAMPLES * SAMPLES)) as u8
}

/// The cover on the transparent canvas, cut to the icon shape.
fn compose(cover: &Rgba) -> Rgba {
    let mut px = vec![0u8; CANVAS * CANVAS * 4];
    let pad = (CANVAS - BODY) / 2;
    for y in 0..BODY {
        for x in 0..BODY {
            let a = coverage(x, y);
            if a == 0 {
                continue;
            }
            let s = &cover.px[(y * BODY + x) * 4..][..4];
            let d = &mut px[((y + pad) * CANVAS + x + pad) * 4..][..4];
            d[..3].copy_from_slice(&s[..3]);
            d[3] = (u16::from(s[3]) * u16::from(a) / 255) as u8;
        }
    }
    Rgba {
        w: CANVAS,
        h: CANVAS,
        px,
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// An RGBA PNG with stored (uncompressed) deflate blocks: `sips` compresses the sizes it writes from it.
fn png(img: &Rgba) -> Vec<u8> {
    let mut raw = Vec::with_capacity((img.w * 4 + 1) * img.h);
    for row in img.px.chunks_exact(img.w * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(0xffff).collect();
    for (i, b) in blocks.iter().enumerate() {
        z.push(u8::from(i + 1 == blocks.len()));
        let n = b.len() as u16;
        z.extend(n.to_le_bytes());
        z.extend((!n).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    z.extend(((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], body: &[u8]| {
        out.extend((body.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        let crc = crc32(&out[start..]);
        out.extend(crc.to_be_bytes());
    };
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend((img.w as u32).to_be_bytes());
    ihdr.extend((img.h as u32).to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shape_is_full_inside_and_empty_in_the_corners() {
        assert_eq!(coverage(BODY / 2, BODY / 2), 255);
        assert_eq!(coverage(BODY / 2, 0), 255);
        assert_eq!(coverage(0, 0), 0);
        assert_eq!(coverage(BODY - 1, BODY - 1), 0);
    }

    #[test]
    fn a_bottom_up_24_bit_bmp_reads_top_down() {
        // 2x2: bottom row blue and red, top row green and white (rows padded to 8 bytes)
        let mut b = b"BM".to_vec();
        b.extend([0; 8]);
        b.extend(54u32.to_le_bytes());
        b.extend(40u32.to_le_bytes());
        b.extend(2i32.to_le_bytes());
        b.extend(2i32.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(24u16.to_le_bytes());
        b.extend([0; 24]);
        b.extend([255, 0, 0, 0, 0, 255, 0, 0]);
        b.extend([0, 255, 0, 255, 255, 255, 0, 0]);
        let img = Rgba::from_bmp(&b).unwrap();
        assert_eq!((img.w, img.h), (2, 2));
        assert_eq!(&img.px[..8], &[0, 255, 0, 255, 255, 255, 255, 255]);
        assert_eq!(&img.px[8..], &[0, 0, 255, 255, 255, 0, 0, 255]);
    }

    #[test]
    fn the_png_has_its_chunks_and_a_valid_crc() {
        let p = png(&Rgba {
            w: 1,
            h: 1,
            px: vec![1, 2, 3, 4],
        });
        assert_eq!(&p[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&p[12..16], b"IHDR");
        assert_eq!(
            crc32(&p[12..29]),
            u32::from_be_bytes(p[29..33].try_into().unwrap())
        );
        assert!(p.ends_with(&[0xae, 0x42, 0x60, 0x82]));
    }
}
