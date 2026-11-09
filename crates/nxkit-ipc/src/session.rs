//! One client session: the root interface, the domain and the dispatch of a request.

use crate::cmif::{self, Call};
use crate::hipc::{self, Parsed, Reply};
use crate::{Inner, STOP_RESULT, Stop};
use nxkit_abi::guest;
use nxkit_abi::ipc::{Handled, Request, Response, Service};
use nxkit_abi::kernel::{ResultCode, Session, kernel};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// The counter name of the control commands.
const CONTROL: &str = "(control)";

/// The result of a request to a session that is closed: `ResultSessionClosed` of the kernel.
const RESULT_SESSION_CLOSED: ResultCode = 0xF601;

/// The largest message that a request reads. The TLS buffer has 0x100 bytes.
const MAX_MESSAGE: u64 = 0x1_0000;

/// The bytes of a message that stay on the stack.
const STACK_MESSAGE: usize = 0x100;

/// The domain of a session: object ID 1 is the root, which `ConvertCurrentObjectToDomain` puts in.
#[derive(Default)]
struct Domain {
    enabled: bool,
    objects: Vec<Option<Arc<dyn Service>>>,
}

impl Domain {
    fn get(&self, id: u32) -> Option<Arc<dyn Service>> {
        self.objects.get(id as usize)?.clone()
    }

    fn add(&mut self, object: Arc<dyn Service>) -> u32 {
        if self.objects.is_empty() {
            self.objects.push(None);
        }
        let free = self.objects.iter().skip(1).position(Option::is_none);
        match free {
            Some(i) => {
                self.objects[i + 1] = Some(object);
                i as u32 + 1
            }
            None => {
                self.objects.push(Some(object));
                self.objects.len() as u32 - 1
            }
        }
    }

    fn remove(&mut self, id: u32) {
        if let Some(slot) = self.objects.get_mut(id as usize) {
            *slot = None;
        }
    }
}

/// What the sessions of one object share: a session and its clones have one domain.
struct Link {
    service: &'static str,
    root: Arc<dyn Service>,
    domain: Mutex<Domain>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A client session as the kernel holds it behind a handle.
pub(crate) struct IpcSession {
    inner: Arc<Inner>,
    link: Arc<Link>,
    closed: AtomicBool,
}

/// A new session for `root`, which belongs to the service `service`.
pub(crate) fn new_session(
    inner: &Arc<Inner>,
    service: &'static str,
    root: Arc<dyn Service>,
) -> Arc<dyn Session> {
    Arc::new(IpcSession::new(inner.clone(), service, root))
}

impl IpcSession {
    fn new(inner: Arc<Inner>, service: &'static str, root: Arc<dyn Service>) -> IpcSession {
        IpcSession {
            inner,
            link: Arc::new(Link {
                service,
                root,
                domain: Mutex::new(Domain::default()),
            }),
            closed: AtomicBool::new(false),
        }
    }

    fn clone_session(&self) -> IpcSession {
        IpcSession {
            inner: self.inner.clone(),
            link: self.link.clone(),
            closed: AtomicBool::new(false),
        }
    }

    /// Records a stop and returns the result that tells the kernel to stop the game (R15).
    fn stop(&self, interface: &str, command: u32, reason: String) -> ResultCode {
        self.inner.stop.record(Stop {
            service: self.link.service.to_string(),
            interface: interface.to_string(),
            command,
            reason,
        });
        STOP_RESULT
    }

    /// The Horizon service a new object belongs to. Objects returned by `sm:` are the roots of the services they were
    /// asked for, so they carry that service's name, not the port's; every other object inherits its parent's.
    fn service_of(&self, iface: &Arc<dyn Service>) -> &'static str {
        if self.link.service != crate::SM_PORT {
            return self.link.service;
        }
        self.inner
            .registry
            .iter()
            .find(|(_, root)| Arc::ptr_eq(root, iface))
            .map_or(self.link.service, |(name, _)| *name)
    }

    /// A handle for a new session of `iface`, which the response moves to the game.
    fn handle_for(&self, iface: Arc<dyn Service>, command: u32) -> Result<u32, ResultCode> {
        let Some(k) = kernel() else {
            return Err(self.stop(
                iface.name(),
                command,
                "the kernel is not registered".to_string(),
            ));
        };
        let session = new_session(&self.inner, self.service_of(&iface), iface);
        k.add_session(session)
    }

    fn dispatch(&self, message: u64, size: u64) -> Result<(), ResultCode> {
        if self.closed.load(Ordering::Acquire) {
            return Err(RESULT_SESSION_CLOSED);
        }
        let len = size.min(MAX_MESSAGE) as usize;
        let mut stack = [0u8; STACK_MESSAGE];
        let mut heap = Vec::new();
        let buf: &mut [u8] = if len <= STACK_MESSAGE {
            &mut stack[..len]
        } else {
            heap.resize(len, 0);
            &mut heap
        };
        let root = self.link.root.name();
        guest::read(message, buf).map_err(|f| self.stop(root, 0, f.to_string()))?;
        let parsed = hipc::parse(buf).map_err(|e| self.stop(root, 0, e.to_string()))?;
        let reply = match parsed.msg_type {
            hipc::TYPE_CLOSE => {
                self.closed.store(true, Ordering::Release);
                return Err(RESULT_SESSION_CLOSED);
            }
            hipc::TYPE_REQUEST | hipc::TYPE_REQUEST_CONTEXT | hipc::TYPE_LEGACY_REQUEST => {
                self.request_message(&parsed)?
            }
            hipc::TYPE_CONTROL | hipc::TYPE_CONTROL_CONTEXT | hipc::TYPE_LEGACY_CONTROL => {
                self.control_message(&parsed)?
            }
            other => {
                return Err(self.stop(
                    root,
                    0,
                    format!("the HIPC message type {other} is not served"),
                ));
            }
        };
        if reply.len() > len {
            return Err(self.stop(
                root,
                0,
                format!(
                    "the reply has {} bytes and the message buffer {len}",
                    reply.len()
                ),
            ));
        }
        guest::write(message, &reply).map_err(|f| self.stop(root, 0, f.to_string()))?;
        Ok(())
    }

    /// A request for the root or for an object of the domain.
    fn request_message(&self, p: &Parsed<'_>) -> Result<Vec<u8>, ResultCode> {
        let root_name = self.link.root.name();
        let domain_on = lock(&self.link.domain).enabled;
        let parsed = if domain_on {
            cmif::parse_domain(p.raw)
        } else {
            cmif::parse_session(p.raw)
        };
        let call: Call<'_> = parsed.map_err(|e| self.stop(root_name, 0, e.to_string()))?;

        let mut in_objects = Vec::with_capacity(call.in_objects.len());
        let target = if domain_on {
            let mut dom = lock(&self.link.domain);
            if call.close {
                dom.remove(call.object_id);
                drop(dom);
                return Ok(hipc::encode(&Reply {
                    result: 0,
                    data: &[],
                    copy_handles: &[],
                    move_handles: &[],
                    domain_objects: Some(&[]),
                }));
            }
            for id in &call.in_objects {
                match dom.get(*id) {
                    Some(o) => in_objects.push(o),
                    None => {
                        drop(dom);
                        return Err(self.stop(
                            root_name,
                            call.command,
                            format!("the request passes the unknown object {id}"),
                        ));
                    }
                }
            }
            match dom.get(call.object_id) {
                Some(t) => t,
                None => {
                    drop(dom);
                    let id = call.object_id;
                    return Err(self.stop(
                        root_name,
                        call.command,
                        format!("the request is for the unknown object {id}"),
                    ));
                }
            }
        } else {
            self.link.root.clone()
        };

        let iface = target.name();
        self.inner.counters.hit(iface, call.command);
        let req = Request {
            command: call.command,
            data: call.data,
            pid: p.pid,
            copy_handles: &p.copy_handles,
            move_handles: &p.move_handles,
            buffers: &p.buffers,
            objects: &in_objects,
        };
        let mut rsp = Response::default();
        if target.handle(&req, &mut rsp) == Handled::Unknown {
            return Err(self.stop(
                iface,
                call.command,
                "the command is not implemented".to_string(),
            ));
        }
        if rsp.result != 0 {
            return Ok(hipc::encode(&Reply {
                result: rsp.result,
                data: &[],
                copy_handles: &[],
                move_handles: &[],
                domain_objects: domain_on.then_some(&[][..]),
            }));
        }

        let mut moved = std::mem::take(&mut rsp.move_handles);
        let mut ids = Vec::new();
        for object in std::mem::take(&mut rsp.objects) {
            if domain_on {
                ids.push(lock(&self.link.domain).add(object));
            } else {
                moved.push(self.handle_for(object, call.command)?);
            }
        }
        Ok(hipc::encode(&Reply {
            result: 0,
            data: &rsp.data,
            copy_handles: &rsp.copy_handles,
            move_handles: &moved,
            domain_objects: domain_on.then_some(ids.as_slice()),
        }))
    }

    /// A control request: it is for the session and not for an interface.
    fn control_message(&self, p: &Parsed<'_>) -> Result<Vec<u8>, ResultCode> {
        let root_name = self.link.root.name();
        let call =
            cmif::parse_session(p.raw).map_err(|e| self.stop(root_name, 0, e.to_string()))?;
        self.inner.counters.hit(CONTROL, call.command);
        let ok = |data: &[u8], moved: &[u32]| {
            hipc::encode(&Reply {
                result: 0,
                data,
                copy_handles: &[],
                move_handles: moved,
                domain_objects: None,
            })
        };
        match call.command {
            cmif::CONTROL_CONVERT_TO_DOMAIN => {
                let mut dom = lock(&self.link.domain);
                if !dom.enabled {
                    dom.enabled = true;
                    dom.objects.push(None);
                    dom.objects.push(Some(self.link.root.clone()));
                }
                Ok(ok(&1u32.to_le_bytes(), &[]))
            }
            cmif::CONTROL_COPY_FROM_DOMAIN => {
                let id = call
                    .data
                    .get(..4)
                    .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
                let object = id.and_then(|id| lock(&self.link.domain).get(id));
                let Some(object) = object else {
                    return Err(self.stop(
                        CONTROL,
                        call.command,
                        format!("CopyFromCurrentDomain names the unknown object {id:?}"),
                    ));
                };
                let h = self.handle_for(object, call.command)?;
                Ok(ok(&[], &[h]))
            }
            cmif::CONTROL_CLONE | cmif::CONTROL_CLONE_EX => {
                let Some(k) = kernel() else {
                    return Err(self.stop(
                        CONTROL,
                        call.command,
                        "the kernel is not registered".to_string(),
                    ));
                };
                let h = k.add_session(Arc::new(self.clone_session()))?;
                Ok(ok(&[], &[h]))
            }
            cmif::CONTROL_QUERY_POINTER_BUFFER_SIZE => Ok(ok(&0u16.to_le_bytes(), &[])),
            other => Err(self.stop(
                CONTROL,
                other,
                "the control command is not served".to_string(),
            )),
        }
    }
}

impl Session for IpcSession {
    fn request(&self, message: u64, size: u64) -> ResultCode {
        match self.dispatch(message, size) {
            Ok(()) => 0,
            Err(code) => code,
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }
}
