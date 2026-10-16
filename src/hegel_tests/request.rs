use std::convert::TryFrom;
use std::mem::size_of;
use std::os::unix::ffi::OsStrExt;

use hegel::TestCase;
use hegel::generators as gs;
use zerocopy::IntoBytes;

use crate::InitFlags;
use crate::LockOwner;
use crate::ll::AnyRequest;
use crate::ll::FileHandle;
use crate::ll::INodeNo;
use crate::ll::Lock;
use crate::ll::Operation;
use crate::ll::TimeOrNow;
use crate::ll::flags::fattr_flags::FattrFlags;
use crate::ll::flags::statx_flags::StatxMask;
use crate::ll::fuse_abi::fuse_in_header;
use crate::ll::fuse_abi::fuse_ioctl_in;
use crate::ll::test::AlignedData;
use crate::time::system_time_from_time;

const KNOWN_OPCODES: &[u32] = &[
    1, 2, 3, 4, 5, 6, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 20, 21, 22, 23, 24, 25, 26, 27, 28,
    29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 50, 51, 52, 4096,
];

fn aligned_bytes(bytes: &[u8]) -> AlignedData<Vec<u64>> {
    let mut words = vec![0; bytes.len().div_ceil(size_of::<u64>())];
    for (index, chunk) in bytes.chunks(8).enumerate() {
        let mut word = [0; 8];
        word[..chunk.len()].copy_from_slice(chunk);
        words[index] = u64::from_ne_bytes(word);
    }
    AlignedData(words)
}

fn request_bytes(opcode: u32, tail: &[u8], len: usize) -> AlignedData<Vec<u64>> {
    let packet_len = len.max(size_of::<fuse_in_header>()).min(4096);
    let mut bytes = vec![0; packet_len];
    bytes[0..4].copy_from_slice(&(packet_len as u32).to_ne_bytes());
    bytes[4..8].copy_from_slice(&opcode.to_ne_bytes());
    let tail_len = tail.len().min(packet_len - size_of::<fuse_in_header>());
    bytes[size_of::<fuse_in_header>()..size_of::<fuse_in_header>() + tail_len]
        .copy_from_slice(&tail[..tail_len]);
    aligned_bytes(&bytes)
}

fn typed_request(opcode: u32, nodeid: u64, tail: &[u8]) -> AlignedData<Vec<u64>> {
    let mut packet = request_bytes(opcode, tail, size_of::<fuse_in_header>() + tail.len());
    let bytes = packet.0.as_mut_bytes();
    bytes[8..16].copy_from_slice(&0x1234_5678_9abc_def0_u64.to_ne_bytes());
    bytes[16..24].copy_from_slice(&nodeid.to_ne_bytes());
    packet
}

fn push_u32(data: &mut Vec<u8>, value: u32) {
    data.extend_from_slice(&value.to_ne_bytes());
}

fn push_i32(data: &mut Vec<u8>, value: i32) {
    data.extend_from_slice(&value.to_ne_bytes());
}

fn push_u64(data: &mut Vec<u8>, value: u64) {
    data.extend_from_slice(&value.to_ne_bytes());
}

fn push_i64(data: &mut Vec<u8>, value: i64) {
    data.extend_from_slice(&value.to_ne_bytes());
}

fn push_name(data: &mut Vec<u8>, name: &[u8]) {
    data.extend_from_slice(name);
    data.push(0);
}

fn random_name(tc: &TestCase) -> Vec<u8> {
    let mut value = tc.draw(gs::vecs(gs::integers::<u8>()).min_size(1).max_size(100));
    value.retain(|byte| *byte != 0 && *byte != b'/');
    if value.is_empty() {
        value.push(b'x');
    }
    value
}

fn parse(packet: &AlignedData<Vec<u64>>) -> (AnyRequest<'_>, Operation<'_>) {
    let request = AnyRequest::try_from(packet.as_bytes()).unwrap();
    let operation = request.operation().unwrap();
    (request, operation)
}

#[hegel::test]
fn arbitrary_aligned_request_bytes_do_not_panic(tc: TestCase) {
    let bytes = tc.draw(gs::vecs(gs::integers::<u8>()).max_size(2048));
    let aligned = aligned_bytes(&bytes);
    if let Ok(request) = AnyRequest::try_from(aligned.as_bytes()) {
        let _ = request.operation();
        let _ = request.to_string();
    }
}

fn ioctl_tail_is_kernel_consistent(tail: &[u8]) -> bool {
    let in_size_offset = std::mem::offset_of!(fuse_ioctl_in, in_size);
    let Some(in_size_bytes) = tail.get(in_size_offset..in_size_offset + size_of::<u32>()) else {
        return true;
    };
    let in_size = u32::from_ne_bytes(in_size_bytes.try_into().unwrap()) as usize;
    in_size <= tail.len().saturating_sub(size_of::<fuse_ioctl_in>())
}

#[hegel::test]
fn every_known_opcode_parser_handles_random_tails(tc: TestCase) {
    let selected = tc.draw(gs::sampled_from(KNOWN_OPCODES));
    let tail = tc.draw(gs::vecs(gs::integers::<u8>()).max_size(320));
    for opcode in KNOWN_OPCODES
        .iter()
        .copied()
        .chain(std::iter::once(selected))
    {
        if opcode == 39 && !ioctl_tail_is_kernel_consistent(&tail) {
            continue;
        }
        let packet = request_bytes(opcode, &tail, 40 + tail.len());
        if let Ok(request) = AnyRequest::try_from(packet.as_bytes()) {
            if let Ok(operation) = request.operation() {
                let _ = operation.to_string();
            }
            let _ = request.to_string();
        }
    }
}

#[hegel::test]
fn lookup_request_preserves_random_name(tc: TestCase) {
    let name = random_name(&tc);
    let mut tail = name.clone();
    tail.push(0);
    let packet = typed_request(1, 1, &tail);
    let (request, operation) = parse(&packet);
    assert_eq!(
        request.unique(),
        crate::ll::RequestId(0x1234_5678_9abc_def0)
    );
    let Operation::Lookup(lookup) = operation else {
        panic!("opcode 1 did not parse as LOOKUP");
    };
    assert_eq!(lookup.name().as_os_str().as_bytes(), name);
}

#[hegel::test]
fn named_request_arguments_roundtrip(tc: TestCase) {
    let nodeid = tc.draw(gs::integers::<u64>());
    let name = random_name(&tc);
    let second = random_name(&tc);
    let target = random_name(&tc);

    let mut args = Vec::new();
    let mode = tc.draw(gs::integers::<u32>());
    let rdev = tc.draw(gs::integers::<u32>());
    let umask = tc.draw(gs::integers::<u32>());
    push_u32(&mut args, mode);
    push_u32(&mut args, rdev);
    push_u32(&mut args, umask);
    push_u32(&mut args, 0);
    push_name(&mut args, &name);
    let packet = typed_request(8, nodeid, &args);
    let (_, operation) = parse(&packet);
    let Operation::MkNod(parsed) = operation else {
        panic!("MKNOD opcode parsed as another operation");
    };
    assert_eq!(parsed.name().as_os_str().as_bytes(), name);
    assert_eq!(parsed.mode(), mode);
    assert_eq!(parsed.rdev(), rdev);
    assert_eq!(parsed.umask(), umask);

    args.clear();
    let mode = tc.draw(gs::integers::<u32>());
    let umask = tc.draw(gs::integers::<u32>());
    push_u32(&mut args, mode);
    push_u32(&mut args, umask);
    push_name(&mut args, &name);
    let packet = typed_request(9, nodeid, &args);
    let (_, operation) = parse(&packet);
    let Operation::MkDir(parsed) = operation else {
        panic!("MKDIR opcode parsed as another operation");
    };
    assert_eq!(parsed.name().as_os_str().as_bytes(), name);
    assert_eq!(parsed.mode(), mode);
    assert_eq!(parsed.umask(), umask);

    args.clear();
    push_name(&mut args, &name);
    push_name(&mut args, &target);
    let packet = typed_request(6, nodeid, &args);
    let (_, operation) = parse(&packet);
    let Operation::SymLink(parsed) = operation else {
        panic!("SYMLINK opcode parsed as another operation");
    };
    assert_eq!(parsed.link_name().as_os_str().as_bytes(), name);
    assert_eq!(parsed.target().as_os_str().as_bytes(), target);

    for opcode in [10, 11, 24] {
        args.clear();
        push_name(&mut args, &name);
        let packet = typed_request(opcode, nodeid, &args);
        let (_, operation) = parse(&packet);
        match (opcode, operation) {
            (10, Operation::Unlink(parsed)) => {
                assert_eq!(parsed.name().as_os_str().as_bytes(), name)
            }
            (11, Operation::RmDir(parsed)) => {
                assert_eq!(parsed.name().as_os_str().as_bytes(), name)
            }
            (24, Operation::RemoveXAttr(parsed)) => {
                assert_eq!(parsed.name().as_bytes(), name)
            }
            _ => panic!("name opcode parsed as another operation"),
        }
    }

    args.clear();
    let newdir = tc.draw(gs::integers::<u64>());
    push_u64(&mut args, newdir);
    #[cfg(target_os = "macos")]
    {
        push_u32(&mut args, 0);
        push_u32(&mut args, 0);
    }
    push_name(&mut args, &name);
    push_name(&mut args, &second);
    let packet = typed_request(12, nodeid, &args);
    let (_, operation) = parse(&packet);
    let Operation::Rename(parsed) = operation else {
        panic!("RENAME opcode parsed as another operation");
    };
    assert_eq!(parsed.src().dir, INodeNo(nodeid));
    assert_eq!(parsed.src().name.as_os_str().as_bytes(), name);
    assert_eq!(parsed.dest().dir, INodeNo(newdir));
    assert_eq!(parsed.dest().name.as_os_str().as_bytes(), second);

    args.clear();
    push_u64(&mut args, newdir);
    push_u32(&mut args, tc.draw(gs::integers::<u32>()));
    push_u32(&mut args, 0);
    push_name(&mut args, &name);
    push_name(&mut args, &second);
    let packet = typed_request(45, nodeid, &args);
    let (_, operation) = parse(&packet);
    let Operation::Rename2(parsed) = operation else {
        panic!("RENAME2 opcode parsed as another operation");
    };
    assert_eq!(parsed.from().dir, INodeNo(nodeid));
    assert_eq!(parsed.from().name.as_os_str().as_bytes(), name);
    assert_eq!(parsed.to().dir, INodeNo(newdir));
    assert_eq!(parsed.to().name.as_os_str().as_bytes(), second);

    args.clear();
    let oldnode = tc.draw(gs::integers::<u64>());
    push_u64(&mut args, oldnode);
    push_name(&mut args, &name);
    let packet = typed_request(13, nodeid, &args);
    let (_, operation) = parse(&packet);
    let Operation::Link(parsed) = operation else {
        panic!("LINK opcode parsed as another operation");
    };
    assert_eq!(parsed.inode_no(), INodeNo(oldnode));
    assert_eq!(parsed.dest().dir, INodeNo(nodeid));
    assert_eq!(parsed.dest().name.as_os_str().as_bytes(), name);

    args.clear();
    let flags = tc.draw(gs::integers::<i32>());
    let mode = tc.draw(gs::integers::<u32>());
    let umask = tc.draw(gs::integers::<u32>());
    let open_flags = tc.draw(gs::integers::<u32>());
    push_i32(&mut args, flags);
    push_u32(&mut args, mode);
    push_u32(&mut args, umask);
    push_u32(&mut args, open_flags);
    push_name(&mut args, &name);
    let packet = typed_request(35, nodeid, &args);
    let (_, operation) = parse(&packet);
    let Operation::Create(parsed) = operation else {
        panic!("CREATE opcode parsed as another operation");
    };
    assert_eq!(parsed.name().as_os_str().as_bytes(), name);
    assert_eq!(parsed.flags(), flags);
    assert_eq!(parsed.mode(), mode);
    assert_eq!(parsed.umask(), umask);

    args.clear();
    let size = tc.draw(gs::integers::<u32>());
    push_u32(&mut args, size);
    push_u32(&mut args, 0);
    push_name(&mut args, &name);
    let packet = typed_request(22, nodeid, &args);
    let (_, operation) = parse(&packet);
    let Operation::GetXAttr(parsed) = operation else {
        panic!("GETXATTR opcode parsed as another operation");
    };
    assert_eq!(parsed.name().as_bytes(), name);
    assert_eq!(parsed.size_u32(), size);

    args.clear();
    let value = tc.draw(gs::vecs(gs::integers::<u8>()).max_size(100));
    let flags = tc.draw(gs::integers::<i32>());
    push_u32(&mut args, u32::try_from(value.len()).unwrap());
    push_i32(&mut args, flags);
    #[cfg(target_os = "macos")]
    {
        push_u32(&mut args, 0);
        push_u32(&mut args, 0);
    }
    push_name(&mut args, &name);
    args.extend_from_slice(&value);
    let packet = typed_request(21, nodeid, &args);
    let (_, operation) = parse(&packet);
    let Operation::SetXAttr(parsed) = operation else {
        panic!("SETXATTR opcode parsed as another operation");
    };
    assert_eq!(parsed.name().as_bytes(), name);
    assert_eq!(parsed.value(), value);
    assert_eq!(parsed.flags(), flags);

    #[cfg(not(target_os = "macos"))]
    {
        args.clear();
        let value = tc.draw(gs::vecs(gs::integers::<u8>()).max_size(100));
        let flags = tc.draw(gs::integers::<i32>());
        let setxattr_flags = tc.draw(gs::integers::<u32>());
        push_u32(&mut args, u32::try_from(value.len()).unwrap());
        push_i32(&mut args, flags);
        push_u32(&mut args, setxattr_flags);
        push_u32(&mut args, 0);
        push_name(&mut args, &name);
        args.extend_from_slice(&value);
        let packet = typed_request(21, nodeid, &args);
        let mut request = AnyRequest::try_from(packet.as_bytes()).unwrap();
        request.set_negotiated(InitFlags::FUSE_SETXATTR_EXT);
        let Operation::SetXAttr(parsed) = request.operation().unwrap() else {
            panic!("extended SETXATTR opcode parsed as another operation");
        };
        assert_eq!(parsed.name().as_bytes(), name);
        assert_eq!(parsed.value(), value);
        assert_eq!(parsed.flags(), flags);
        assert_eq!(parsed.setxattr_flags(), setxattr_flags);
    }

    args.clear();
    let mode = tc.draw(gs::integers::<u32>());
    let umask = tc.draw(gs::integers::<u32>());
    let flags = tc.draw(gs::integers::<i32>());
    let open_flags = tc.draw(gs::integers::<u32>());
    push_i32(&mut args, flags);
    push_u32(&mut args, mode);
    push_u32(&mut args, umask);
    push_u32(&mut args, open_flags);
    push_name(&mut args, b"/");
    let packet = typed_request(51, nodeid, &args);
    let (_, operation) = parse(&packet);
    let Operation::TmpFile(parsed) = operation else {
        panic!("TMPFILE opcode parsed as another operation");
    };
    assert_eq!(parsed.mode(), mode);
    assert_eq!(parsed.umask(), umask);
    assert_eq!(parsed.flags(), flags);
}

#[hegel::test]
fn request_struct_fields_and_offsets_roundtrip(tc: TestCase) {
    let nodeid = tc.draw(gs::integers::<u64>());
    let fh = tc.draw(gs::integers::<u64>());
    let offset = tc.draw(gs::integers::<i64>().min_value(0));
    let size = tc.draw(gs::integers::<u32>());
    let mut args = Vec::new();

    let read_flags = tc.draw(gs::integers::<u32>());
    let lock_owner = tc.draw(gs::integers::<u64>());
    push_u64(&mut args, fh);
    push_u64(&mut args, u64::try_from(offset).unwrap());
    push_u32(&mut args, size);
    push_u32(&mut args, read_flags);
    push_u64(&mut args, lock_owner);
    push_i32(&mut args, 0);
    push_u32(&mut args, 0);
    let packet = typed_request(15, nodeid, &args);
    let (_, operation) = parse(&packet);
    let Operation::Read(parsed) = operation else {
        panic!("READ opcode parsed as another operation");
    };
    assert_eq!(parsed.file_handle(), FileHandle(fh));
    assert_eq!(parsed.offset(), Ok(u64::try_from(offset).unwrap()));
    assert_eq!(parsed.size(), size);
    assert_eq!(parsed.read_flags().bits(), read_flags);

    args.clear();
    let data = tc.draw(gs::vecs(gs::integers::<u8>()).max_size(100));
    let write_flags = tc.draw(gs::integers::<u32>());
    push_u64(&mut args, fh);
    push_i64(&mut args, offset);
    push_u32(&mut args, u32::try_from(data.len()).unwrap());
    push_u32(&mut args, write_flags);
    push_u64(&mut args, lock_owner);
    push_i32(&mut args, 0);
    push_u32(&mut args, 0);
    args.extend_from_slice(&data);
    let packet = typed_request(16, nodeid, &args);
    let (_, operation) = parse(&packet);
    let Operation::Write(parsed) = operation else {
        panic!("WRITE opcode parsed as another operation");
    };
    assert_eq!(parsed.file_handle(), FileHandle(fh));
    assert_eq!(parsed.offset(), Ok(u64::try_from(offset).unwrap()));
    assert_eq!(parsed.data(), data);
    assert_eq!(parsed.write_flags().bits(), write_flags);

    args.clear();
    let mode = tc.draw(gs::integers::<i32>());
    let max_off = u64::try_from(i64::MAX).unwrap();
    let length = tc.draw(gs::integers::<u64>().max_value(max_off));
    push_u64(&mut args, fh);
    push_u64(&mut args, u64::try_from(offset).unwrap());
    push_u64(&mut args, length);
    push_i32(&mut args, mode);
    push_u32(&mut args, 0);
    let packet = typed_request(43, nodeid, &args);
    let (_, operation) = parse(&packet);
    let Operation::FAllocate(parsed) = operation else {
        panic!("FALLOCATE opcode parsed as another operation");
    };
    assert_eq!(parsed.file_handle(), FileHandle(fh));
    assert_eq!(parsed.offset(), Ok(u64::try_from(offset).unwrap()));
    assert_eq!(parsed.len(), Ok(length));
    assert_eq!(parsed.mode(), mode);

    args.clear();
    let seek_offset = tc.draw(gs::integers::<i64>());
    let whence = tc.draw(gs::integers::<i32>());
    push_u64(&mut args, fh);
    push_i64(&mut args, seek_offset);
    push_i32(&mut args, whence);
    push_u32(&mut args, 0);
    let packet = typed_request(46, nodeid, &args);
    let (_, operation) = parse(&packet);
    let Operation::Lseek(parsed) = operation else {
        panic!("LSEEK opcode parsed as another operation");
    };
    assert_eq!(parsed.file_handle(), FileHandle(fh));
    assert_eq!(parsed.offset(), seek_offset);
    assert_eq!(parsed.whence(), whence);

    args.clear();
    let source_offset = tc.draw(gs::integers::<i64>().min_value(0));
    let dest_offset = tc.draw(gs::integers::<i64>().min_value(0));
    let dest_node = tc.draw(gs::integers::<u64>());
    let dest_fh = tc.draw(gs::integers::<u64>());
    let length = tc.draw(gs::integers::<u64>());
    let copy_flags = tc.draw(gs::integers::<u64>());
    push_u64(&mut args, fh);
    push_i64(&mut args, source_offset);
    push_u64(&mut args, dest_node);
    push_u64(&mut args, dest_fh);
    push_i64(&mut args, dest_offset);
    push_u64(&mut args, length);
    push_u64(&mut args, copy_flags);
    let packet = typed_request(47, nodeid, &args);
    let (_, operation) = parse(&packet);
    let Operation::CopyFileRange(parsed) = operation else {
        panic!("COPY_FILE_RANGE opcode parsed as another operation");
    };
    assert_eq!(parsed.src().unwrap().inode, INodeNo(nodeid));
    assert_eq!(parsed.src().unwrap().file_handle, FileHandle(fh));
    assert_eq!(
        parsed.src().unwrap().offset,
        u64::try_from(source_offset).unwrap()
    );
    assert_eq!(parsed.dest().unwrap().inode, INodeNo(dest_node));
    assert_eq!(parsed.dest().unwrap().file_handle, FileHandle(dest_fh));
    assert_eq!(
        parsed.dest().unwrap().offset,
        u64::try_from(dest_offset).unwrap()
    );
    assert_eq!(parsed.len(), length);
    assert_eq!(parsed.flags().bits(), copy_flags);

    args.clear();
    let ioctl_flags = tc.draw(gs::integers::<u32>());
    let command = tc.draw(gs::integers::<u32>());
    let ioctl_data = tc.draw(gs::vecs(gs::integers::<u8>()).max_size(100));
    let out_size = tc.draw(gs::integers::<u32>());
    push_u64(&mut args, fh);
    push_u32(&mut args, ioctl_flags);
    push_u32(&mut args, command);
    push_u64(&mut args, 0);
    push_u32(&mut args, u32::try_from(ioctl_data.len()).unwrap());
    push_u32(&mut args, out_size);
    args.extend_from_slice(&ioctl_data);
    let packet = typed_request(39, nodeid, &args);
    let (_, operation) = parse(&packet);
    let Operation::IoCtl(parsed) = operation else {
        panic!("IOCTL opcode parsed as another operation");
    };
    assert_eq!(parsed.file_handle(), FileHandle(fh));
    assert_eq!(parsed.flags().bits(), ioctl_flags);
    assert_eq!(parsed.command(), command);
    assert_eq!(parsed.in_data(), ioctl_data);
    assert_eq!(parsed.out_size(), out_size);

    args.clear();
    let poll_handle = tc.draw(gs::integers::<u64>());
    let poll_flags = tc.draw(gs::integers::<u32>());
    let events = tc.draw(gs::integers::<u32>());
    push_u64(&mut args, fh);
    push_u64(&mut args, poll_handle);
    push_u32(&mut args, poll_flags);
    push_u32(&mut args, events);
    let packet = typed_request(40, nodeid, &args);
    let (_, operation) = parse(&packet);
    let Operation::Poll(parsed) = operation else {
        panic!("POLL opcode parsed as another operation");
    };
    assert_eq!(parsed.file_handle(), FileHandle(fh));
    assert_eq!(parsed.kernel_handle().0, poll_handle);
    assert_eq!(parsed.flags().bits(), poll_flags);
    assert_eq!(parsed.events().bits(), events);

    args.clear();
    let start = tc.draw(gs::integers::<u64>());
    let end = tc.draw(gs::integers::<u64>());
    let lock_type = tc.draw(gs::integers::<i32>());
    let pid = tc.draw(gs::integers::<u32>());
    let owner = tc.draw(gs::integers::<u64>());
    let lock_flags = tc.draw(gs::integers::<u32>());
    let lock = Lock {
        range: (start, end),
        typ: lock_type,
        pid,
    };
    push_u64(&mut args, fh);
    push_u64(&mut args, owner);
    push_u64(&mut args, start);
    push_u64(&mut args, end);
    push_i32(&mut args, lock_type);
    push_u32(&mut args, pid);
    push_u32(&mut args, lock_flags);
    push_u32(&mut args, 0);
    let packet = typed_request(31, nodeid, &args);
    let (_, operation) = parse(&packet);
    let Operation::GetLk(parsed) = operation else {
        panic!("GETLK opcode parsed as another operation");
    };
    assert_eq!(parsed.file_handle(), FileHandle(fh));
    assert_eq!(parsed.lock_owner(), LockOwner(owner));
    assert_eq!(parsed.lock(), lock);

    for (opcode, expected_wait) in [(32, false), (33, true)] {
        let packet = typed_request(opcode, nodeid, &args);
        let (_, operation) = parse(&packet);
        let Operation::SetLk(parsed) = operation else {
            panic!("SETLK opcode parsed as another operation");
        };
        assert_eq!(parsed.file_handle(), FileHandle(fh));
        assert_eq!(parsed.lock_owner(), LockOwner(owner));
        assert_eq!(parsed.lock(), lock);
        assert_eq!(parsed.sleep(), expected_wait);
    }
}

#[hegel::test]
fn setattr_and_directory_request_fields_roundtrip(tc: TestCase) {
    let nodeid = tc.draw(gs::integers::<u64>());
    let file_handle = tc.draw(gs::integers::<u64>());
    let mode = tc.draw(gs::integers::<u32>());
    let uid = tc.draw(gs::integers::<u32>());
    let gid = tc.draw(gs::integers::<u32>());
    let size = tc.draw(gs::integers::<u64>());
    let atime_secs = tc.draw(gs::integers::<i64>().min_value(-1_000_000_000));
    let mtime_secs = tc.draw(gs::integers::<i64>().min_value(-1_000_000_000));
    let ctime_secs = tc.draw(gs::integers::<i64>().min_value(-1_000_000_000));
    let atime_nsecs = tc.draw(gs::integers::<u32>().max_value(999_999_999));
    let mtime_nsecs = tc.draw(gs::integers::<u32>().max_value(999_999_999));
    let ctime_nsecs = tc.draw(gs::integers::<u32>().max_value(999_999_999));
    let valid = FattrFlags::FATTR_MODE
        | FattrFlags::FATTR_UID
        | FattrFlags::FATTR_GID
        | FattrFlags::FATTR_SIZE
        | FattrFlags::FATTR_ATIME
        | FattrFlags::FATTR_MTIME
        | FattrFlags::FATTR_CTIME
        | FattrFlags::FATTR_FH;
    let mut args = Vec::new();
    push_u32(&mut args, valid.bits());
    push_u32(&mut args, 0);
    push_u64(&mut args, file_handle);
    push_u64(&mut args, size);
    push_u64(&mut args, 0);
    push_i64(&mut args, atime_secs);
    push_i64(&mut args, mtime_secs);
    push_i64(&mut args, ctime_secs);
    push_u32(&mut args, atime_nsecs);
    push_u32(&mut args, mtime_nsecs);
    push_u32(&mut args, ctime_nsecs);
    push_u32(&mut args, mode);
    push_u32(&mut args, 0);
    push_u32(&mut args, uid);
    push_u32(&mut args, gid);
    push_u32(&mut args, 0);
    #[cfg(target_os = "macos")]
    args.resize(size_of::<abi::fuse_setattr_in>(), 0);
    let packet = typed_request(4, nodeid, &args);
    let (_, operation) = parse(&packet);
    let Operation::SetAttr(parsed) = operation else {
        panic!("SETATTR opcode parsed as another operation");
    };
    assert_eq!(parsed.mode(), Some(mode));
    assert_eq!(parsed.uid(), Some(uid));
    assert_eq!(parsed.gid(), Some(gid));
    assert_eq!(parsed.size(), Some(size));
    assert_eq!(parsed.file_handle(), Some(FileHandle(file_handle)));
    assert_eq!(
        parsed.atime(),
        Some(TimeOrNow::SpecificTime(system_time_from_time(
            atime_secs,
            atime_nsecs
        )))
    );
    assert_eq!(
        parsed.mtime(),
        Some(TimeOrNow::SpecificTime(system_time_from_time(
            mtime_secs,
            mtime_nsecs
        )))
    );
    assert_eq!(
        parsed.ctime(),
        Some(system_time_from_time(ctime_secs, ctime_nsecs))
    );

    let fh = tc.draw(gs::integers::<u64>());
    let offset = tc.draw(gs::integers::<u64>());
    let read_size = tc.draw(gs::integers::<u32>());
    let mut read_args = Vec::new();
    push_u64(&mut read_args, fh);
    push_u64(&mut read_args, offset);
    push_u32(&mut read_args, read_size);
    push_u32(&mut read_args, 0);
    push_u64(&mut read_args, 0);
    push_i32(&mut read_args, 0);
    push_u32(&mut read_args, 0);
    for opcode in [28, 44] {
        let packet = typed_request(opcode, nodeid, &read_args);
        let (_, operation) = parse(&packet);
        match (opcode, operation) {
            (28, Operation::ReadDir(parsed)) => {
                assert_eq!(parsed.file_handle(), FileHandle(fh));
                assert_eq!(parsed.offset(), offset);
                assert_eq!(parsed.size(), read_size);
            }
            (44, Operation::ReadDirPlus(parsed)) => {
                assert_eq!(parsed.file_handle(), FileHandle(fh));
                assert_eq!(parsed.offset(), offset);
                assert_eq!(parsed.size(), read_size);
            }
            _ => panic!("READDIR opcode parsed as another operation"),
        }
    }

    let statx_flags = tc.draw(gs::integers::<u32>());
    let statx_mask = tc.draw(gs::integers::<u32>());
    let mut statx_args = Vec::new();
    push_u32(&mut statx_args, 0);
    push_u32(&mut statx_args, 0);
    push_u64(&mut statx_args, file_handle);
    push_u32(&mut statx_args, statx_flags);
    push_u32(&mut statx_args, statx_mask);
    let packet = typed_request(52, nodeid, &statx_args);
    let (_, operation) = parse(&packet);
    let Operation::StatX(parsed) = operation else {
        panic!("STATX opcode parsed as another operation");
    };
    assert_eq!(parsed.file_handle(), None);
    assert_eq!(parsed.flags(), statx_flags);
    assert_eq!(parsed.mask(), StatxMask::from_bits_retain(statx_mask));
}

#[hegel::test]
fn batch_forget_node_records_roundtrip(tc: TestCase) {
    let count = tc.draw(gs::integers::<u32>().max_value(20));
    let mut args = Vec::new();
    push_u32(&mut args, count);
    push_u32(&mut args, 0);
    let nodes: Vec<_> = (0..count)
        .map(|_| {
            let nodeid = tc.draw(gs::integers::<u64>());
            let nlookup = tc.draw(gs::integers::<u64>());
            push_u64(&mut args, nodeid);
            push_u64(&mut args, nlookup);
            (nodeid, nlookup)
        })
        .collect();
    let packet = typed_request(42, 1, &args);
    let (_, operation) = parse(&packet);
    let Operation::BatchForget(parsed) = operation else {
        panic!("BATCH_FORGET opcode parsed as another operation");
    };
    assert_eq!(
        parsed
            .nodes()
            .iter()
            .map(|node| (node.nodeid, node.nlookup))
            .collect::<Vec<_>>(),
        nodes
    );
}
