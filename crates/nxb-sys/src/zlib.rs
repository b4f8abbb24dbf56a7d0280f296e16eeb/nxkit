//! The system zlib (`/usr/lib/libz`, linked dynamically): raw deflate and inflate with Python's `zlib` semantics, so
//! a deflate here is byte-identical to `zlib.compressobj(level, DEFLATED, -15)` in the same process family (Python on
//! macOS links the same library, checked by the IPA test in nxb-pack).

use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void};

// Links the static zlib that `libz-sys` builds (Windows has none of its own).
#[cfg(windows)]
use libz_sys as _;

#[repr(C)]
struct ZStream {
    next_in: *const u8,
    avail_in: c_uint,
    total_in: c_ulong,
    next_out: *mut u8,
    avail_out: c_uint,
    total_out: c_ulong,
    msg: *const c_char,
    state: *mut c_void,
    zalloc: *const c_void,
    zfree: *const c_void,
    opaque: *mut c_void,
    data_type: c_int,
    adler: c_ulong,
    reserved: c_ulong,
}

#[cfg_attr(unix, link(name = "z"))]
unsafe extern "C" {
    fn zlibVersion() -> *const c_char;
    fn deflateInit2_(
        s: *mut ZStream,
        level: c_int,
        method: c_int,
        wbits: c_int,
        mem: c_int,
        strategy: c_int,
        v: *const c_char,
        size: c_int,
    ) -> c_int;
    fn deflate(s: *mut ZStream, flush: c_int) -> c_int;
    fn deflateEnd(s: *mut ZStream) -> c_int;
    fn inflateInit2_(s: *mut ZStream, wbits: c_int, v: *const c_char, size: c_int) -> c_int;
    fn inflate(s: *mut ZStream, flush: c_int) -> c_int;
    fn inflateEnd(s: *mut ZStream) -> c_int;
}

const Z_NO_FLUSH: c_int = 0;
const Z_FINISH: c_int = 4;
const Z_OK: c_int = 0;
const Z_STREAM_END: c_int = 1;
const Z_BUF_ERROR: c_int = -5;
const CHUNK: usize = 1 << 16;

fn zeroed() -> ZStream {
    ZStream {
        next_in: std::ptr::null(),
        avail_in: 0,
        total_in: 0,
        next_out: std::ptr::null_mut(),
        avail_out: 0,
        total_out: 0,
        msg: std::ptr::null(),
        state: std::ptr::null_mut(),
        zalloc: std::ptr::null(),
        zfree: std::ptr::null(),
        opaque: std::ptr::null_mut(),
        data_type: 0,
        adler: 0,
        reserved: 0,
    }
}

/// The runtime library's version (`zlib.ZLIB_RUNTIME_VERSION`).
pub fn version() -> String {
    // SAFETY: zlibVersion returns a static NUL-terminated string.
    unsafe { std::ffi::CStr::from_ptr(zlibVersion()) }
        .to_string_lossy()
        .into_owned()
}

/// A deflate stream (`zlib.compressobj(level, DEFLATED, wbits)`, memLevel 8, default strategy).
pub struct Deflate {
    s: Box<ZStream>,
}

// SAFETY: the stream is owned and used from one thread at a time (&mut self).
unsafe impl Send for Deflate {}

impl Deflate {
    /// `compressobj(level, DEFLATED, wbits)`.`wbits` -15 gives raw deflate.
    pub fn new(level: i32, wbits: i32) -> Option<Deflate> {
        let mut s = Box::new(zeroed());
        // SAFETY: s is a zeroed z_stream (null allocators = defaults), version and size describe this layout.
        let r = unsafe {
            deflateInit2_(
                &mut *s,
                level,
                8,
                wbits,
                8,
                0,
                zlibVersion(),
                size_of::<ZStream>() as c_int,
            )
        };
        (r == Z_OK).then_some(Deflate { s })
    }

    fn run(&mut self, data: &[u8], flush: c_int, out: &mut Vec<u8>) -> bool {
        let mut buf = vec![0u8; CHUNK];
        let mut pos = 0usize;
        loop {
            let take = (data.len() - pos).min(c_uint::MAX as usize);
            self.s.next_in = data[pos..].as_ptr();
            self.s.avail_in = take as c_uint;
            self.s.next_out = buf.as_mut_ptr();
            self.s.avail_out = CHUNK as c_uint;
            // SAFETY: next_in/next_out point at live buffers of the given lengths for the duration of the call.
            let r = unsafe { deflate(&mut *self.s, flush) };
            let used = take - self.s.avail_in as usize;
            pos += used;
            out.extend_from_slice(&buf[..CHUNK - self.s.avail_out as usize]);
            if r == Z_STREAM_END {
                return true;
            }
            if r != Z_OK && r != Z_BUF_ERROR {
                return false;
            }
            if flush == Z_NO_FLUSH && pos == data.len() && self.s.avail_out != 0 {
                return true;
            }
        }
    }

    /// `compress(data)`: deflate with no flush.
    pub fn compress(&mut self, data: &[u8]) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        self.run(data, Z_NO_FLUSH, &mut out).then_some(out)
    }

    /// `flush()`: the rest of the stream (Z_FINISH).
    pub fn finish(mut self) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        self.run(&[], Z_FINISH, &mut out).then_some(out)
    }
}

impl Drop for Deflate {
    fn drop(&mut self) {
        // SAFETY: the stream was initialised by deflateInit2_.
        unsafe { deflateEnd(&mut *self.s) };
    }
}

/// `c = compressobj(level, DEFLATED, wbits); c.compress(data) + c.flush()`.
pub fn deflate_bytes(data: &[u8], level: i32, wbits: i32) -> Option<Vec<u8>> {
    let mut d = Deflate::new(level, wbits)?;
    let mut out = d.compress(data)?;
    out.extend(d.finish()?);
    Some(out)
}

/// Inflates a stream (`wbits` -15: raw deflate) to at most `limit` bytes.`None` when invalid or longer.
pub fn inflate_bytes(data: &[u8], wbits: i32, limit: usize) -> Option<Vec<u8>> {
    let mut s = Box::new(zeroed());
    // SAFETY: as in Deflate::new.
    if unsafe { inflateInit2_(&mut *s, wbits, zlibVersion(), size_of::<ZStream>() as c_int) }
        != Z_OK
    {
        return None;
    }
    let mut out = Vec::new();
    let mut buf = vec![0u8; CHUNK];
    s.next_in = data.as_ptr();
    s.avail_in = data.len().min(c_uint::MAX as usize) as c_uint;
    let ok = loop {
        s.next_out = buf.as_mut_ptr();
        s.avail_out = CHUNK as c_uint;
        // SAFETY: the buffers outlive the call.
        let r = unsafe { inflate(&mut *s, Z_NO_FLUSH) };
        out.extend_from_slice(&buf[..CHUNK - s.avail_out as usize]);
        if out.len() > limit {
            break false;
        }
        match r {
            Z_STREAM_END => break true,
            Z_OK if s.avail_in > 0 || s.avail_out == 0 => continue,
            _ => break false,
        }
    };
    // SAFETY: initialised above.
    unsafe { inflateEnd(&mut *s) };
    ok.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_version() {
        assert!(version().starts_with('1'));
        let data: Vec<u8> = (0..200_000u32)
            .map(|i| ((i % 251) ^ (i / 7)) as u8)
            .collect();
        let z = deflate_bytes(&data, 6, -15).unwrap_or_default();
        assert!(!z.is_empty() && z.len() < data.len());
        assert_eq!(inflate_bytes(&z, -15, 1 << 20), Some(data));
        // Python: zlib.compressobj(6, zlib.DEFLATED, -15), c.compress(b"a") + c.flush() == b"K\x04\x00"
        assert_eq!(deflate_bytes(b"a", 6, -15), Some(vec![0x4b, 0x04, 0x00]));
    }
}
