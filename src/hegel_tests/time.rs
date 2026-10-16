use hegel::TestCase;
use hegel::generators as gs;

use crate::time::system_time_from_time;
use crate::time::time_from_system_time;

#[hegel::test]
fn time_tuple_roundtrips(tc: TestCase) {
    let secs = tc.draw(
        gs::integers::<i64>()
            .min_value(-1_000_000_000_000)
            .max_value(1_000_000_000_000),
    );
    let nsecs = tc.draw(gs::integers::<u32>().max_value(999_999_999));
    let time = system_time_from_time(secs, nsecs);
    assert_eq!((secs, nsecs), time_from_system_time(&time));
}

#[hegel::test]
fn system_time_roundtrips(tc: TestCase) {
    let secs = tc.draw(
        gs::integers::<i64>()
            .min_value(-1_000_000_000_000)
            .max_value(1_000_000_000_000),
    );
    let nsecs = tc.draw(gs::integers::<u32>().max_value(999_999_999));
    let time = system_time_from_time(secs, nsecs);
    assert_eq!(
        time,
        system_time_from_time(
            time_from_system_time(&time).0,
            time_from_system_time(&time).1
        )
    );
}

#[hegel::test]
fn time_conversion_is_monotonic(tc: TestCase) {
    let first = tc.draw(
        gs::integers::<i64>()
            .min_value(-1_000_000_000)
            .max_value(1_000_000_000),
    );
    let second = tc.draw(
        gs::integers::<i64>()
            .min_value(first)
            .max_value(1_000_000_000),
    );
    let first_time = system_time_from_time(first, 0);
    let second_time = system_time_from_time(second, 999_999_999);
    assert!(first_time <= second_time);
    assert!(time_from_system_time(&first_time) <= time_from_system_time(&second_time));
}
