//! `nn::diag` take-overs: the game log and the aborts. Source: nxnative `dm/host/hle_sdk16.c`, section "diag".
//!
//! An abort prints the Result and ends the process with a failure. The call site of nxnative comes from the guest
//! return address, which an export cannot read, so the message has no call site.

use super::{fatal, get_bytes, get_cstr, get_u32, log, zero};

/// `PutImpl(const LogMetaData&, const char*, size_t)`: the text goes to the host log.
extern "C" fn put(_meta: u64, text: u64, len: u64) {
    let mut n = usize::try_from(len).unwrap_or(0).min(0x10000);
    if text == 0 {
        return;
    }
    let bytes = get_bytes(text, n);
    while n > 0 && matches!(bytes[n - 1], b'\n' | 0) {
        n -= 1;
    }
    if n > 0 {
        log(&format!("[game] {}", String::from_utf8_lossy(&bytes[..n])));
    }
}

fn abort(result: u32) -> ! {
    fatal(&format!("game called nn::diag abort: Result {result:#x}"))
}

/// `AbortImpl()`.
extern "C" fn abort_plain() {
    abort(0)
}

/// `AbortImpl(const Result*)`.
extern "C" fn abort_result(result: u64) {
    abort(if result == 0 { 0 } else { get_u32(result) })
}

/// `VAbortImpl(...)`: prints the place and the format of the message. The arguments of the format are not expanded,
/// because a `va_list` that the guest built is not readable on the host.
#[allow(clippy::too_many_arguments)]
extern "C" fn v_abort(
    func: u64,
    expr: u64,
    file: u64,
    line: u32,
    result: u64,
    _info: u64,
    format: u64,
    _ap: u64,
) {
    let (func, expr, file, format) = (
        get_cstr(func, 256),
        get_cstr(expr, 256),
        get_cstr(file, 256),
        get_cstr(format, 256),
    );
    log(&format!(
        "[diag] abort at {func}: {expr} ({file}:{}) message format {format}",
        line as i32
    ));
    abort(if result == 0 { 0 } else { get_u32(result) })
}

/// `GetSymbolName(char*, size_t, uint64_t)`: no symbols, an empty string.
extern "C" fn symbol_name(buf: u64, n: u64) -> u64 {
    if buf != 0 && n != 0 {
        super::put_u8(buf, 0);
    }
    0
}

table! {
    "_ZN2nn4diag6detail7PutImplERKNS0_11LogMetaDataEPKcm" => put,
    "_ZN2nn4diag6detail9AbortImplEv" => abort_plain,
    "_ZN2nn4diag6detail9AbortImplEPKNS_6ResultE" => abort_result,
    "_ZN2nn4diag12GetBacktraceEPmi" => zero,
    "_ZN2nn4diag6detail10VAbortImplEPKcS3_S3_iPKNS_6ResultEPKNS_2os17UserExceptionInfoES3_RSt9__va_list" => v_abort,
    "_ZN2nn4diag13GetSymbolNameEPcmm" => symbol_name,
}
