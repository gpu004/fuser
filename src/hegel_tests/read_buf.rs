use std::mem::align_of;

use hegel::TestCase;
use hegel::generators as gs;

use crate::ll::fuse_abi::fuse_in_header;
use crate::read_buf::BUFFER_SIZE;
use crate::read_buf::FuseReadBuf;

#[hegel::test]
fn read_buffer_is_aligned_and_large_enough(_tc: TestCase) {
    let mut buffer = FuseReadBuf::new();
    let bytes = buffer.as_mut();
    assert_eq!((bytes.as_ptr() as usize) % align_of::<fuse_in_header>(), 0);
    assert!(bytes.len() >= BUFFER_SIZE - 7);
}

#[hegel::test]
fn read_buffer_alignment_survives_repeated_mutation(tc: TestCase) {
    let mut buffer = FuseReadBuf::new();
    let len = tc.draw(
        gs::integers::<usize>()
            .min_value(0)
            .max_value(BUFFER_SIZE - 8),
    );
    buffer.as_mut()[..len].fill(0xa5);
    let bytes = buffer.as_mut();
    assert_eq!((bytes.as_ptr() as usize) % align_of::<fuse_in_header>(), 0);
    assert!(bytes.len() >= BUFFER_SIZE - 7);
}
