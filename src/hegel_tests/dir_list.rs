use std::ffi::OsStr;
use std::mem::size_of;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::Duration;
use std::time::UNIX_EPOCH;

use hegel::TestCase;
use hegel::generators as gs;

use crate::FileAttr;
use crate::FileType;
use crate::Generation;
use crate::INodeNo;
use crate::ll::RequestId;
use crate::ll::ResponseData;
use crate::ll::fuse_abi as abi;
use crate::ll::reply::Attr;
use crate::ll::reply::DirEntList;
use crate::ll::reply::DirEntOffset;
use crate::ll::reply::DirEntPlusList;
use crate::ll::reply::DirEntry;
use crate::ll::reply::DirEntryPlus;
use crate::ll::reply::Response;

#[derive(Clone, Debug, Eq, PartialEq)]
struct Entry {
    ino: u64,
    offset: u64,
    kind: FileType,
    name: Vec<u8>,
}

fn file_type_number(kind: FileType) -> u32 {
    match kind {
        FileType::NamedPipe => 1,
        FileType::CharDevice => 2,
        FileType::Directory => 4,
        FileType::BlockDevice => 6,
        FileType::RegularFile => 8,
        FileType::Symlink => 10,
        FileType::Socket => 12,
    }
}

fn name(tc: &TestCase) -> Vec<u8> {
    let mut bytes = tc.draw(gs::vecs(gs::integers::<u8>()).min_size(1).max_size(300));
    bytes.retain(|byte| *byte != 0 && *byte != b'/');
    if bytes.is_empty() {
        bytes.push(b'x');
    }
    bytes
}

fn file_attr(ino: u64, kind: FileType) -> FileAttr {
    FileAttr {
        ino: INodeNo(ino),
        size: 0,
        blocks: 0,
        atime: UNIX_EPOCH,
        mtime: UNIX_EPOCH,
        ctime: UNIX_EPOCH,
        crtime: UNIX_EPOCH,
        kind,
        perm: 0o644,
        nlink: 1,
        uid: 0,
        gid: 0,
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}

fn response_bytes(response: &impl Response) -> Vec<u8> {
    response.with_iovec(RequestId(1), |iov| {
        iov.iter().flat_map(|part| part.iter().copied()).collect()
    })
}

fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_ne_bytes(data[offset..offset + 8].try_into().unwrap())
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_ne_bytes(data[offset..offset + 4].try_into().unwrap())
}

fn verify_dirents(mut bytes: &[u8], entries: &[Entry], header_size: usize, dirent_offset: usize) {
    for entry in entries {
        let name_len = entry.name.len();
        let item_len = (header_size + name_len).next_multiple_of(8);
        if bytes.len() < item_len {
            break;
        }
        assert_eq!(read_u64(bytes, dirent_offset), entry.ino);
        assert_eq!(read_u64(bytes, dirent_offset + 8), entry.offset);
        assert_eq!(read_u32(bytes, dirent_offset + 16) as usize, name_len);
        assert_eq!(
            read_u32(bytes, dirent_offset + 20),
            file_type_number(entry.kind)
        );
        assert_eq!(&bytes[header_size..header_size + name_len], entry.name);
        assert!(
            bytes[header_size + name_len..item_len]
                .iter()
                .all(|byte| *byte == 0)
        );
        bytes = &bytes[item_len..];
    }
    assert_eq!(bytes.len(), 0);
}

#[hegel::test]
fn directory_lists_encode_only_entries_that_fit(tc: TestCase) {
    let max_size = tc.draw(gs::integers::<usize>().max_value(8192));
    let count = tc.draw(gs::integers::<usize>().max_value(25));
    let kinds = [
        FileType::NamedPipe,
        FileType::CharDevice,
        FileType::BlockDevice,
        FileType::Directory,
        FileType::RegularFile,
        FileType::Symlink,
        FileType::Socket,
    ];
    let mut expected = Vec::new();
    let mut expected_plus = Vec::new();
    let mut list = DirEntList::new(max_size);
    let mut plus = DirEntPlusList::new(max_size);
    let mut used = 0;
    let mut used_plus = 0;
    for _ in 0..count {
        let ino = tc.draw(gs::integers::<u64>());
        let offset = tc.draw(gs::integers::<u64>());
        let kind = tc.draw(gs::sampled_from(kinds.to_vec()));
        let bytes = name(&tc);
        let path = PathBuf::from(OsStr::from_bytes(&bytes));
        let item = Entry {
            ino,
            offset,
            kind,
            name: bytes,
        };
        let padded_len = (size_of::<abi::fuse_dirent>() + item.name.len()).next_multiple_of(8);
        let padded_plus_len =
            (size_of::<abi::fuse_direntplus>() + item.name.len()).next_multiple_of(8);
        let remaining = max_size - used;
        let remaining_plus = max_size - used_plus;
        let dirent = DirEntry::new(INodeNo(ino), DirEntOffset(offset), kind, &path);
        let direntplus = DirEntryPlus::new(
            INodeNo(ino),
            Generation(0),
            DirEntOffset(offset),
            &path,
            Duration::ZERO,
            Attr::from(file_attr(ino, kind)),
            Duration::ZERO,
        );
        let full = list.push(&dirent);
        let plus_full = plus.push(&direntplus);
        assert_eq!(full, padded_len > remaining);
        assert_eq!(plus_full, padded_plus_len > remaining_plus);
        if full {
            assert_eq!(list.remaining_capacity(), remaining);
        } else {
            used += padded_len;
            assert_eq!(list.remaining_capacity(), max_size - used);
            expected.push(item.clone());
        }
        if plus_full {
            assert_eq!(plus.remaining_capacity(), remaining_plus);
        } else {
            used_plus += padded_plus_len;
            assert_eq!(plus.remaining_capacity(), max_size - used_plus);
            expected_plus.push(item);
        }
    }
    let plain = response_bytes(&ResponseData::from(list));
    let plus_bytes = response_bytes(&ResponseData::from(plus));
    assert_eq!(plain.len() - 16, used);
    assert_eq!(plus_bytes.len() - 16, used_plus);
    verify_dirents(&plain[16..], &expected, size_of::<abi::fuse_dirent>(), 0);
    verify_dirents(
        &plus_bytes[16..],
        &expected_plus,
        size_of::<abi::fuse_direntplus>(),
        size_of::<abi::fuse_entry_out>(),
    );
}
