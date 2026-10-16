use std::io::IoSlice;

use hegel::TestCase;
use hegel::generators as gs;

use crate::Errno;
use crate::ll::RequestId;
use crate::ll::ResponseData;
use crate::ll::ResponseErrno;
use crate::ll::ResponseIoctl;
use crate::ll::ResponseSlice;
use crate::ll::ResponseStruct;
use crate::ll::fuse_abi as abi;
use crate::ll::reply::Response;

fn check_reply(response: &impl Response, errno: i32, payload_len: usize) {
    response.with_iovec(RequestId(0x1122_3344), |iov| {
        let bytes: Vec<u8> = iov.iter().flat_map(|part| part.iter().copied()).collect();
        assert_eq!(bytes.len(), size_of::<abi::fuse_out_header>() + payload_len);
        assert_eq!(
            u32::from_ne_bytes(bytes[..4].try_into().unwrap()) as usize,
            bytes.len()
        );
        assert_eq!(i32::from_ne_bytes(bytes[4..8].try_into().unwrap()), errno);
        assert_eq!(
            u64::from_ne_bytes(bytes[8..16].try_into().unwrap()),
            0x1122_3344
        );
    });
}

#[hegel::test]
fn response_iovec_headers_match_payloads(tc: TestCase) {
    let payload = tc.draw(gs::vecs(gs::integers::<u8>()).max_size(1024));
    check_reply(&ResponseSlice(&payload), 0, payload.len());
    check_reply(&ResponseData::new_data(payload.clone()), 0, payload.len());
    let record = abi::fuse_write_out {
        size: tc.draw(gs::integers::<u32>()),
        padding: tc.draw(gs::integers::<u32>()),
    };
    check_reply(&ResponseStruct(record), 0, size_of::<abi::fuse_write_out>());
    check_reply(&ResponseErrno(Errno::EIO), -libc::EIO, 0);
    let ioctl_data = payload.as_slice();
    let ioctl_iov = [IoSlice::new(ioctl_data)];
    check_reply(
        &ResponseIoctl::new_ioctl(tc.draw(gs::integers::<i32>()), &ioctl_iov),
        0,
        size_of::<abi::fuse_ioctl_out>() + payload.len(),
    );
}
