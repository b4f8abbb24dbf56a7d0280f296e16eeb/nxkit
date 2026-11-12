//! Small C ABI for the Swift app shell; the native engine remains Rust.

#[cfg(target_os = "ios")]
#[allow(unsafe_code)]
mod ffi;

/// Copies a UTF-8 message without splitting a code point, always NUL terminating
/// when space exists. No allocation ownership crosses the ABI.
fn error_bytes(message: &str, out: &mut [u8]) {
    if out.is_empty() {
        return;
    }
    let mut end = message.len().min(out.len() - 1);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    out[..end].copy_from_slice(&message.as_bytes()[..end]);
    out[end] = 0;
}
fn valid_touches(points: &[crate::api::Touch]) -> bool {
    let mut ids = 0u16;
    points.len() <= 16
        && points.iter().all(|p| {
            if p.id >= 16
                || !p.x.is_finite()
                || !p.y.is_finite()
                || !(0.0..=1.0).contains(&p.x)
                || !(0.0..=1.0).contains(&p.y)
            {
                return false;
            }
            let bit = 1u16 << p.id;
            if ids & bit != 0 {
                return false;
            }
            ids |= bit;
            true
        })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Touch;
    #[test]
    fn errors_are_bounded_and_utf8() {
        for length in 0..12 {
            let mut out = vec![0xff; length];
            error_bytes("aé語🙂z", &mut out);
            if length > 0 {
                let end = out.iter().position(|&b| b == 0).unwrap();
                assert!(std::str::from_utf8(&out[..end]).is_ok());
            }
        }
    }
    #[test]
    fn touch_abi_is_three_c_words() {
        assert_eq!(std::mem::size_of::<Touch>(), 12);
        assert_eq!(std::mem::align_of::<Touch>(), 4);
        assert_eq!(std::mem::offset_of!(Touch, id), 0);
        assert_eq!(std::mem::offset_of!(Touch, x), 4);
        assert_eq!(std::mem::offset_of!(Touch, y), 8);
    }
    #[test]
    fn rejects_duplicate_nonfinite_or_out_of_range_touches() {
        let p = Touch {
            id: 0,
            x: 0.5,
            y: 1.0,
        };
        assert!(valid_touches(&[]));
        assert!(valid_touches(&[p]));
        assert!(!valid_touches(&[p, p]));
        for bad in [
            Touch { id: 16, ..p },
            Touch { x: f32::NAN, ..p },
            Touch { x: -0.1, ..p },
            Touch { y: 1.1, ..p },
        ] {
            assert!(!valid_touches(&[bad]));
        }
    }
}
