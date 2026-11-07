use super::*;
use crate::hipc;
use nxkit_abi::guest;
use nxkit_abi::ipc::{Handled, Request, Response};
use nxkit_abi::kernel::{Kernel, set_kernel};
use std::sync::OnceLock;

/// The guest range is global, so the tests that use it run one at a time.
static GUEST: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct TestKernel {
    sessions: Mutex<Vec<Arc<dyn Session>>>,
}

impl Kernel for TestKernel {
    fn create_event(&self) -> Result<(u32, u32), ResultCode> {
        Err(1)
    }
    fn signal_event(&self, _handle: u32) -> Result<(), ResultCode> {
        Err(1)
    }
    fn clear_event(&self, _handle: u32) -> Result<(), ResultCode> {
        Err(1)
    }
    fn add_session(&self, session: Arc<dyn Session>) -> Result<u32, ResultCode> {
        let mut s = self.sessions.lock().unwrap();
        s.push(session);
        Ok(0x100 + s.len() as u32 - 1)
    }
    fn create_shared_memory(&self, _size: u64) -> Result<(u32, u64), ResultCode> {
        Err(1)
    }
    fn close_handle(&self, _handle: u32) -> Result<(), ResultCode> {
        Ok(())
    }
}

fn test_kernel() -> &'static Arc<TestKernel> {
    static K: OnceLock<Arc<TestKernel>> = OnceLock::new();
    K.get_or_init(|| {
        let k = Arc::new(TestKernel::default());
        set_kernel(k.clone());
        k
    })
}

fn session_at(handle: u32) -> Arc<dyn Session> {
    test_kernel().sessions.lock().unwrap()[(handle - 0x100) as usize].clone()
}

/// Command 1 adds one to the first word, command 2 returns another `Echo`, every other command is unknown.
struct Echo(&'static str);

impl Service for Echo {
    fn name(&self) -> &'static str {
        self.0
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            1 => {
                rsp.push_u32(req.u32_at(0).unwrap_or(0) + 1);
                Handled::Done
            }
            2 => {
                rsp.objects.push(Arc::new(Echo("Child")));
                Handled::Done
            }
            3 => {
                rsp.result = 0x202;
                Handled::Done
            }
            _ => Handled::Unknown,
        }
    }
}

fn ipc() -> Ipc {
    Ipc::new(vec![Named {
        name: "echo",
        root: Arc::new(Echo("Echo")),
    }])
    .unwrap()
}

fn header(w0: u32, words: u32) -> Vec<u8> {
    let mut m = Vec::new();
    m.extend_from_slice(&w0.to_le_bytes());
    m.extend_from_slice(&words.to_le_bytes());
    m.resize(16, 0);
    m
}

fn sfci(command: u32, data: &[u8]) -> Vec<u8> {
    let mut m = Vec::new();
    for w in [0x4943_4653u32, 0, command, 0] {
        m.extend_from_slice(&w.to_le_bytes());
    }
    m.extend_from_slice(data);
    m
}

/// A request message of a session: no descriptors, the CMIF header and `data`.
fn request(command: u32, data: &[u8]) -> Vec<u8> {
    let body = sfci(command, data);
    let mut m = header(4, 2 + body.len() as u32 / 4);
    m.extend(body);
    m
}

fn control(command: u32, data: &[u8]) -> Vec<u8> {
    let mut m = request(command, data);
    m[0] = 5;
    m
}

/// A request message in a domain for `object`.
fn domain_request(object: u32, command: u32, data: &[u8]) -> Vec<u8> {
    let body = sfci(command, data);
    let mut m = header(4, 2 + 4 + body.len() as u32 / 4);
    m.extend_from_slice(&[1, 0]);
    m.extend_from_slice(&(body.len() as u16).to_le_bytes());
    m.extend_from_slice(&object.to_le_bytes());
    m.extend_from_slice(&[0; 8]);
    m.extend(body);
    m
}

/// Sends `msg` and returns the result of `request` and the parsed reply.
fn send(s: &dyn Session, mem: &[u8], msg: &[u8]) -> (ResultCode, Vec<u8>) {
    let base = mem.as_ptr() as u64;
    guest::fill(base, 0x100, 0).unwrap();
    guest::write(base, msg).unwrap();
    let rc = s.request(base, 0x100);
    let mut out = vec![0u8; 0x100];
    guest::read(base, &mut out).unwrap();
    (rc, out)
}

fn reply_result(out: &[u8]) -> (u32, Vec<u8>, Vec<u32>) {
    let p = hipc::parse(out).unwrap();
    let result = u32::from_le_bytes([p.raw[8], p.raw[9], p.raw[10], p.raw[11]]);
    (result, p.raw[16..].to_vec(), p.move_handles)
}

fn with_memory<R>(f: impl FnOnce(&[u8]) -> R) -> R {
    let _g = GUEST.lock().unwrap_or_else(PoisonError::into_inner);
    test_kernel();
    let mem = vec![0u8; 0x100];
    guest::set_range(mem.as_ptr() as u64, 0x100);
    let r = f(&mem);
    guest::set_range(0, 0);
    r
}

fn get_service(ipc: &Ipc, mem: &[u8], name: &[u8; 8]) -> (ResultCode, Option<u32>) {
    let sm = ipc.server().connect_port("sm:").unwrap();
    let (rc, out) = send(&*sm, mem, &request(sm::GET_SERVICE, name));
    if rc != 0 {
        return (rc, None);
    }
    let (result, _, moved) = reply_result(&out);
    assert_eq!(result, 0);
    (rc, moved.first().copied())
}

#[test]
fn sm_gives_a_session_for_a_registered_service() {
    with_memory(|mem| {
        let ipc = ipc();
        let (rc, h) = get_service(&ipc, mem, b"echo\0\0\0\0");
        assert_eq!(rc, 0);
        let s = session_at(h.unwrap());
        let (rc, out) = send(&*s, mem, &request(1, &41u32.to_le_bytes()));
        assert_eq!(rc, 0);
        let (result, data, _) = reply_result(&out);
        assert_eq!(result, 0);
        assert_eq!(&data[..4], 42u32.to_le_bytes());
        assert!(ipc.stop().is_none());
        let counts = ipc.counts();
        assert!(counts.iter().any(|c| c.interface == "Echo" && c.command == 1 && c.count == 1));
        assert!(counts.iter().any(|c| c.interface == "sm:" && c.command == 1));
        assert!(ipc.dump().contains("Echo\t1\t1"));
    });
}

#[test]
fn an_unknown_service_name_stops_the_game() {
    with_memory(|mem| {
        let ipc = ipc();
        let (rc, h) = get_service(&ipc, mem, b"nope\0\0\0\0");
        assert_eq!((rc, h), (STOP_RESULT, None));
        let stop = ipc.stop().unwrap();
        assert_eq!((stop.service.as_str(), stop.command), ("nope", 1));
    });
}

#[test]
fn an_unknown_command_stops_the_game_with_the_interface_and_the_command() {
    with_memory(|mem| {
        let ipc = ipc();
        let (_, h) = get_service(&ipc, mem, b"echo\0\0\0\0");
        let s = session_at(h.unwrap());
        let (rc, _) = send(&*s, mem, &request(77, &[]));
        assert_eq!(rc, STOP_RESULT);
        let stop = ipc.stop().unwrap();
        assert_eq!(stop.service, "echo");
        assert_eq!(stop.interface, "Echo");
        assert_eq!(stop.command, 77);
    });
}

#[test]
fn a_service_error_is_a_result_and_no_stop() {
    with_memory(|mem| {
        let ipc = ipc();
        let (_, h) = get_service(&ipc, mem, b"echo\0\0\0\0");
        let s = session_at(h.unwrap());
        let (rc, out) = send(&*s, mem, &request(3, &[]));
        assert_eq!(rc, 0);
        assert_eq!(reply_result(&out).0, 0x202);
        assert!(ipc.stop().is_none());
    });
}

#[test]
fn an_object_outside_a_domain_is_a_new_session() {
    with_memory(|mem| {
        let ipc = ipc();
        let (_, h) = get_service(&ipc, mem, b"echo\0\0\0\0");
        let s = session_at(h.unwrap());
        let (_, out) = send(&*s, mem, &request(2, &[]));
        let (result, _, moved) = reply_result(&out);
        assert_eq!((result, moved.len()), (0, 1));
        let child = session_at(moved[0]);
        let (_, out) = send(&*child, mem, &request(1, &1u32.to_le_bytes()));
        assert_eq!(&reply_result(&out).1[..4], 2u32.to_le_bytes());
        assert!(ipc.counts().iter().any(|c| c.interface == "Child" || c.interface == "Echo"));
    });
}

#[test]
fn a_domain_keeps_its_objects_by_id() {
    with_memory(|mem| {
        let ipc = ipc();
        let (_, h) = get_service(&ipc, mem, b"echo\0\0\0\0");
        let s = session_at(h.unwrap());
        let (rc, out) = send(&*s, mem, &control(cmif::CONTROL_CONVERT_TO_DOMAIN, &[]));
        assert_eq!(rc, 0);
        assert_eq!(&reply_result(&out).1[..4], 1u32.to_le_bytes());

        // The reply of a domain has `{ count, 3 words }` before the `SFCO` header.
        let (_, out) = send(&*s, mem, &domain_request(1, 2, &[]));
        let p = hipc::parse(&out).unwrap();
        assert_eq!(&p.raw[..4], 1u32.to_le_bytes());
        assert_eq!(&p.raw[16..20], b"SFCO");
        assert_eq!(&p.raw[32..36], 2u32.to_le_bytes());

        // Object 2 is the child, which counts 1 + 1.
        let (_, out) = send(&*s, mem, &domain_request(2, 1, &1u32.to_le_bytes()));
        let p = hipc::parse(&out).unwrap();
        assert_eq!(&p.raw[32..36], 2u32.to_le_bytes());
        assert!(ipc.counts().iter().any(|c| c.interface == "Child" && c.command == 1));

        // A request for an object that does not exist stops the game.
        let (rc, _) = send(&*s, mem, &domain_request(9, 1, &[0; 4]));
        assert_eq!(rc, STOP_RESULT);
    });
}

#[test]
fn the_pointer_buffer_size_is_zero() {
    with_memory(|mem| {
        let ipc = ipc();
        let (_, h) = get_service(&ipc, mem, b"echo\0\0\0\0");
        let s = session_at(h.unwrap());
        let (rc, out) = send(&*s, mem, &control(cmif::CONTROL_QUERY_POINTER_BUFFER_SIZE, &[]));
        assert_eq!(rc, 0);
        assert_eq!(&reply_result(&out).1[..2], [0, 0]);
    });
}

#[test]
fn two_services_with_one_name_are_an_error() {
    let a = Named { name: "echo", root: Arc::new(Echo("A")) };
    let b = Named { name: "echo", root: Arc::new(Echo("B")) };
    assert_eq!(Ipc::new(vec![a, b]).err(), Some(DuplicateService("echo")));
}

#[test]
fn an_unknown_port_stops_the_game() {
    let ipc = ipc();
    assert_eq!(ipc.server().connect_port("ldr:ro").err(), Some(STOP_RESULT));
    assert_eq!(ipc.stop().unwrap().service, "ldr:ro");
}
