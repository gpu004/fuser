use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use hegel::TestCase;
use hegel::generators as gs;

use crate::INodeNo;
use crate::ll::fuse_abi as abi;
use crate::ll::notify::Notification;
use crate::notify::PollHandle;

fn bytes(notification: &Notification<'_>, code: abi::fuse_notify_code) -> Vec<u8> {
    notification
        .with_iovec(code, |iov| {
            iov.iter().flat_map(|part| part.iter().copied()).collect()
        })
        .unwrap()
}

fn verify_header(bytes: &[u8], code: i32) {
    assert_eq!(
        u32::from_ne_bytes(bytes[..4].try_into().unwrap()) as usize,
        bytes.len()
    );
    assert_eq!(i32::from_ne_bytes(bytes[4..8].try_into().unwrap()), code);
    assert_eq!(u64::from_ne_bytes(bytes[8..16].try_into().unwrap()), 0);
}

#[hegel::test]
fn notifications_have_consistent_headers_lengths_and_names(tc: TestCase) {
    let ino = tc.draw(gs::integers::<u64>());
    let offset = tc.draw(gs::integers::<i64>());
    let len = tc.draw(gs::integers::<i64>());
    let name = tc
        .draw(gs::vecs(gs::integers::<u8>()).max_size(255))
        .into_iter()
        .filter(|byte| *byte != 0)
        .collect::<Vec<_>>();
    let data = tc.draw(gs::vecs(gs::integers::<u8>()).max_size(1024));

    let inval_entry =
        Notification::new_inval_entry(inode(ino), OsStr::from_bytes(&name), 0).unwrap();
    let inval_entry_bytes = bytes(&inval_entry, abi::fuse_notify_code::FUSE_NOTIFY_INVAL_ENTRY);
    verify_header(
        &inval_entry_bytes,
        abi::fuse_notify_code::FUSE_NOTIFY_INVAL_ENTRY as i32,
    );
    assert_eq!(
        u32::from_ne_bytes(inval_entry_bytes[24..28].try_into().unwrap()) as usize,
        name.len()
    );
    assert_eq!(&inval_entry_bytes[32..32 + name.len()], name);
    assert_eq!(inval_entry_bytes[32 + name.len()], 0);

    let delete = Notification::new_delete(
        inode(ino),
        inode(ino.wrapping_add(1)),
        OsStr::from_bytes(&name),
    )
    .unwrap();
    let delete_bytes = bytes(&delete, abi::fuse_notify_code::FUSE_NOTIFY_DELETE);
    verify_header(
        &delete_bytes,
        abi::fuse_notify_code::FUSE_NOTIFY_DELETE as i32,
    );
    assert_eq!(
        u32::from_ne_bytes(delete_bytes[32..36].try_into().unwrap()) as usize,
        name.len()
    );
    assert_eq!(&delete_bytes[40..40 + name.len()], name);
    assert_eq!(delete_bytes[40 + name.len()], 0);

    let store = Notification::new_store(inode(ino), ino, &data).unwrap();
    let store_bytes = bytes(&store, abi::fuse_notify_code::FUSE_NOTIFY_STORE);
    verify_header(
        &store_bytes,
        abi::fuse_notify_code::FUSE_NOTIFY_STORE as i32,
    );
    assert_eq!(
        u32::from_ne_bytes(store_bytes[32..36].try_into().unwrap()) as usize,
        data.len()
    );
    assert_eq!(&store_bytes[40..], data);

    let inval_inode = Notification::new_inval_inode(inode(ino), offset, len);
    let inval_inode_bytes = bytes(&inval_inode, abi::fuse_notify_code::FUSE_NOTIFY_INVAL_INODE);
    verify_header(
        &inval_inode_bytes,
        abi::fuse_notify_code::FUSE_NOTIFY_INVAL_INODE as i32,
    );

    let poll = Notification::new_poll(PollHandle(ino));
    let poll_bytes = bytes(&poll, abi::fuse_notify_code::FUSE_POLL);
    verify_header(&poll_bytes, abi::fuse_notify_code::FUSE_POLL as i32);

    let inc_epoch = Notification::new_inc_epoch();
    let inc_epoch_bytes = bytes(&inc_epoch, abi::fuse_notify_code::FUSE_NOTIFY_INC_EPOCH);
    verify_header(
        &inc_epoch_bytes,
        abi::fuse_notify_code::FUSE_NOTIFY_INC_EPOCH as i32,
    );
    assert_eq!(inc_epoch_bytes.len(), 16);
}

#[hegel::test]
fn notification_lengths_equal_iovec_payload_lengths(tc: TestCase) {
    let bytes = tc.draw(gs::vecs(gs::integers::<u8>()).max_size(1024));
    let notification = Notification::new_store(INodeNo(0), 0, &bytes).unwrap();
    let packet = notification
        .with_iovec(abi::fuse_notify_code::FUSE_NOTIFY_STORE, |iov| {
            iov.iter()
                .flat_map(|part| part.iter().copied())
                .collect::<Vec<_>>()
        })
        .unwrap();
    verify_header(&packet, abi::fuse_notify_code::FUSE_NOTIFY_STORE as i32);
    assert_eq!(packet.len(), 16 + 24 + bytes.len());
}

fn inode(ino: u64) -> INodeNo {
    INodeNo(ino)
}
