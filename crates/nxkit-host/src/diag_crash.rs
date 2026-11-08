#![allow(unsafe_code, missing_docs, clippy::all)]
#[repr(C)]
struct SigAction { handler: usize, mask: u32, flags: i32 }
unsafe extern "C" {
    fn sigaction(sig: i32, act: *const SigAction, old: *mut SigAction) -> i32;
    fn write(fd: i32, buf: *const u8, n: usize) -> isize;
    fn _exit(code: i32) -> !;
}
fn out(s: &str) { unsafe { write(2, s.as_ptr(), s.len()); } }
fn ok(a: u64) -> bool { (0x7000000000..0x7100000000).contains(&a) }
extern "C" fn on_fault(sig: i32, _info: usize, uc: usize) {
    unsafe {
        let mc = *((uc + 48) as *const usize);
        let r = |o: usize| *((mc + o) as *const u64);
        let mut s = format!("DIAG fault sig {sig} far {:#x} pc {:#x} lr {:#x} fp {:#x} sp {:#x}\n", r(0), r(272), r(256), r(248), r(264));
        for i in 0..29 { s += &format!("x{i}={:#x} ", r(16 + i * 8)); }
        s += "\n";
        let rd = |a: u64| if ok(a) { *(a as *const u64) } else { 0 };
        let g = rd(0x7001e57bd8);
        let m = rd(g + 0x370);
        let n = rd(m + 0x478);
        s += &format!("CHAIN g {g:#x} m {m:#x} n {n:#x}\n");
        if ok(n) { for o in (0..0x40).step_by(8) { s += &format!("  n+{o:#x} = {:#x}\n", rd(n + o)); } }
        if ok(m) { for o in (0x400..0x4a0).step_by(8) { let q = rd(m + o); s += &format!("  obj m+{o:#x} {q:#x}: {:#x} {:#x} {:#x} {:#x}\n", rd(q), rd(q + 8), rd(q + 0x10), rd(q + 0x38)); } }
        if false { for o in (0x460..0x4a0).step_by(8) { s += &format!("  m+{o:#x} = {:#x}\n", rd(m + o)); } }
        let sp = r(264);
        if ok(sp) {
            let mut a = sp.saturating_sub(0x200);
            while a < sp + 0x400 {
                let v = *(a as *const u64);
                if ok(v) { s += &format!("  [sp{:+#x}] {:#x}\n", a as i64 - sp as i64, v); }
                a += 8;
            }
        }
        let mut fp = r(248);
        for _ in 0..24 {
            if !ok(fp) { break; }
            let lr = *((fp + 8) as *const u64);
            s += &format!("  frame lr {lr:#x}\n");
            fp = *(fp as *const u64);
        }
        out(&s);
        _exit(139);
    }
}
pub fn install() {
    let a = SigAction { handler: on_fault as usize, mask: 0, flags: 0x40 };
    unsafe { sigaction(11, &a, std::ptr::null_mut()); sigaction(10, &a, std::ptr::null_mut()); sigaction(4, &a, std::ptr::null_mut()); }
}
