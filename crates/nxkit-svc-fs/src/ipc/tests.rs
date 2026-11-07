use super::*;
use crate::files::{MODE_READ, MODE_WRITE};
use crate::result::{PATH_NOT_FOUND, TARGET_NOT_FOUND};
use crate::testutil::{Guest, global};
use nxkit_abi::ipc::{Service, Handled};
use std::sync::Arc;

fn call(s: &dyn Service, command: u32, data: &[u8], buffers: &[Buffer]) -> (Handled, Response) {
    let req = Request { command, data, pid: None, copy_handles: &[], move_handles: &[], buffers, objects: &[] };
    let mut rsp = Response::default();
    let h = s.handle(&req, &mut rsp);
    (h, rsp)
}

fn pointer(g: &Guest, off: u64, index: u32, text: &str) -> Buffer {
    g.cstr(off, text);
    Buffer { addr: g.addr() + off, size: 0x301, kind: BufferKind::Pointer, index }
}

fn receive(g: &Guest, off: u64, size: u64) -> Buffer {
    Buffer { addr: g.addr() + off, size, kind: BufferKind::Receive, index: 0 }
}

fn send(g: &Guest, off: u64, size: u64) -> Buffer {
    Buffer { addr: g.addr() + off, size, kind: BufferKind::Send, index: 0 }
}

fn u32_data(v: u32) -> Vec<u8> {
    let mut d = v.to_le_bytes().to_vec();
    d.resize(8, 0);
    d
}

fn file_data(offset: u64, size: u64) -> Vec<u8> {
    let mut d = vec![0u8; 24];
    d[8..16].copy_from_slice(&offset.to_le_bytes());
    d[16..24].copy_from_slice(&size.to_le_bytes());
    d
}

fn cache_attr(index: u16) -> Vec<u8> {
    let mut d = vec![0u8; 0x48];
    d[0] = 1;
    d[0x28] = 5;
    d[0x2A..0x2C].copy_from_slice(&index.to_le_bytes());
    d
}

#[test]
fn an_unknown_command_is_unknown() {
    global();
    let root = FspSrv::new();
    assert_eq!(call(&root, 9999, &[], &[]).0, Handled::Unknown);
    let (h, rsp) = call(&root, 53, &[0; 0x48], &[]);
    assert_eq!((h, rsp.result), (Handled::Done, TARGET_NOT_FOUND));
}

#[test]
fn a_cache_file_system_creates_writes_and_reads_a_file() {
    global();
    let g = Guest::new(0x2000);
    let root = FspSrv::new();
    let (h, mut rsp) = call(&root, 51, &cache_attr(7), &[]);
    assert_eq!((h, rsp.result), (Handled::Done, 0));
    let fs = rsp.objects.remove(0);
    assert_eq!(fs.name(), "IFileSystem");

    let path = pointer(&g, 0, 0, "/ipc.bin");
    let mut create = u32_data(0);
    create.extend_from_slice(&6u64.to_le_bytes());
    assert_eq!(call(&*fs, 0, &create, &[path]).1.result, 0);
    assert_eq!(call(&*fs, 0, &create, &[path]).1.result, crate::result::PATH_EXISTS);
    assert_eq!(call(&*fs, 7, &[], &[path]).1.data, 1u32.to_le_bytes());
    assert_eq!(call(&*fs, 7, &[], &[pointer(&g, 0x40, 0, "/none")]).1.result, PATH_NOT_FOUND);

    let (_, mut rsp) = call(&*fs, 8, &u32_data(MODE_READ | MODE_WRITE), &[path]);
    assert_eq!(rsp.result, 0);
    let file = rsp.objects.remove(0);
    assert_eq!(file.name(), "IFile");
    g.put(0x400, b"hello!");
    assert_eq!(call(&*file, 1, &file_data(0, 6), &[send(&g, 0x400, 6)]).1.result, 0);
    assert_eq!(call(&*file, 4, &[], &[]).1.data, 6u64.to_le_bytes());
    let (_, rsp) = call(&*file, 0, &file_data(1, 4), &[receive(&g, 0x500, 4)]);
    assert_eq!((rsp.result, rsp.data), (0, 4u64.to_le_bytes().to_vec()));
    assert_eq!(g.bytes(0x500, 4), b"ello");
    assert_eq!(call(&*file, 3, &2u64.to_le_bytes(), &[]).1.result, 0);
    assert_eq!(call(&*file, 4, &[], &[]).1.data, 2u64.to_le_bytes());
    assert_eq!(call(&*file, 77, &[], &[]).0, Handled::Unknown);
    drop(file);

    let (_, mut rsp) = call(&*fs, 9, &u32_data(3), &[pointer(&g, 0x80, 0, "/")]);
    assert_eq!(rsp.result, 0);
    let dir = rsp.objects.remove(0);
    assert_eq!(call(&*dir, 1, &[], &[]).1.data, 1u64.to_le_bytes());
    let (_, rsp) = call(&*dir, 0, &[], &[receive(&g, 0x800, 0x310 * 4)]);
    assert_eq!(rsp.data, 1u64.to_le_bytes());
    assert_eq!(&g.bytes(0x800, 7)[..], b"ipc.bin");

    assert_eq!(call(&*fs, 1, &[], &[path]).1.result, 0);
    assert_eq!(call(&*fs, 10, &[], &[]).1.result, 0);
    assert_eq!(call(&*fs, 99, &[], &[]).0, Handled::Unknown);
}

#[test]
fn the_data_storage_reads_the_romfs_image() {
    global();
    let g = Guest::new(0x1000);
    let root = FspSrv::new();
    let (_, mut rsp) = call(&root, 200, &[], &[]);
    assert_eq!(rsp.result, 0);
    let storage: Arc<dyn Service> = rsp.objects.remove(0);
    assert_eq!(storage.name(), "IStorage");
    let size = u64::from_le_bytes(call(&*storage, 4, &[], &[]).1.data.try_into().unwrap());
    assert!(size > 0x50);
    let mut data = 0u64.to_le_bytes().to_vec();
    data.extend_from_slice(&8u64.to_le_bytes());
    assert_eq!(call(&*storage, 0, &data, &[receive(&g, 0, 8)]).1.result, 0);
    assert_eq!(g.u64_at(0), 0x50);
    let mut past = size.to_le_bytes().to_vec();
    past.extend_from_slice(&8u64.to_le_bytes());
    assert_eq!(call(&*storage, 0, &past, &[receive(&g, 0, 8)]).1.result, FAIL);
}

#[test]
fn the_info_reader_lists_the_save_once() {
    let g = Guest::new(0x400);
    let reader = storage::ISaveDataInfoReader::new(0x0100_7EF0_0011_E000, true);
    let buf = receive(&g, 0, 0x60 * 4);
    assert_eq!(call(&reader, 0, &[], &[buf]).1.data, 1u64.to_le_bytes());
    assert_eq!(g.u64_at(0x28), 0x0100_7EF0_0011_E000);
    assert_eq!(call(&reader, 0, &[], &[buf]).1.data, 0u64.to_le_bytes());
    let empty = storage::ISaveDataInfoReader::new(1, false);
    assert_eq!(call(&empty, 0, &[], &[buf]).1.data, 0u64.to_le_bytes());
}
