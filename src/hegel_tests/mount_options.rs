use hegel::TestCase;
use hegel::generators as gs;

use crate::Config;
use crate::MountOption;
use crate::mnt::mount_options::check_option_conflicts;
use crate::mnt::mount_options::check_option_values;
use crate::mnt::mount_options::option_to_escaped_string;
use crate::mnt::mount_options::option_to_string;

fn draw_value(tc: &TestCase) -> String {
    tc.draw(gs::vecs(gs::integers::<u8>().max_value(127)).max_size(40))
        .into_iter()
        .map(char::from)
        .collect()
}

fn draw_option(tc: &TestCase) -> MountOption {
    let variant = tc.draw(gs::integers::<u8>().max_value(17));
    let value = draw_value(tc);
    match variant {
        0 => MountOption::FSName(value.replace('\0', "_")),
        1 => MountOption::Subtype(safe_value(value)),
        2 => MountOption::CUSTOM(format!("custom-{}", safe_value(value))),
        3 => MountOption::AutoUnmount,
        4 => MountOption::DefaultPermissions,
        5 => MountOption::Dev,
        6 => MountOption::NoDev,
        7 => MountOption::Suid,
        8 => MountOption::NoSuid,
        9 => MountOption::RO,
        10 => MountOption::RW,
        11 => MountOption::Exec,
        12 => MountOption::NoExec,
        13 => MountOption::Atime,
        14 => MountOption::NoAtime,
        15 => MountOption::DirSync,
        16 => MountOption::Sync,
        _ => MountOption::Async,
    }
}

fn safe_value(value: String) -> String {
    value
        .chars()
        .map(|character| {
            if matches!(character, '\0' | ',' | '\\') {
                '_'
            } else {
                character
            }
        })
        .collect()
}

fn unescape_options(joined: &str) -> Vec<String> {
    if joined.is_empty() {
        return Vec::new();
    }
    let mut options = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for character in joined.chars() {
        if escaped {
            current.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == ',' {
            options.push(std::mem::take(&mut current));
        } else {
            current.push(character);
        }
    }
    if escaped {
        current.push('\\');
    }
    options.push(current);
    options
}

#[hegel::test]
fn escaped_mount_options_roundtrip_through_fusermount_splitting(tc: TestCase) {
    let count = tc.draw(gs::integers::<usize>().max_value(30));
    let options: Vec<_> = (0..count).map(|_| draw_option(&tc)).collect();
    let accepted: Vec<_> = options
        .into_iter()
        .filter(|option| check_option_values(std::slice::from_ref(option)).is_ok())
        .collect();
    let encoded = accepted
        .iter()
        .map(option_to_escaped_string)
        .collect::<Vec<_>>()
        .join(",");
    let decoded = unescape_options(&encoded);
    let expected = accepted.iter().map(option_to_string).collect::<Vec<_>>();
    assert_eq!(decoded, expected);
    assert_eq!(
        decoded
            .iter()
            .map(|option| crate::mnt::mount_options::MountOption::from_str(option))
            .collect::<Vec<_>>(),
        accepted
    );
}

#[hegel::test]
fn every_mount_option_formats_back_to_its_variant(tc: TestCase) {
    for option in [
        MountOption::FSName(draw_value(&tc).replace('\0', "_")),
        MountOption::Subtype(safe_value(draw_value(&tc))),
        MountOption::CUSTOM(format!("custom-{}", safe_value(draw_value(&tc)))),
        MountOption::AutoUnmount,
        MountOption::DefaultPermissions,
        MountOption::Dev,
        MountOption::NoDev,
        MountOption::Suid,
        MountOption::NoSuid,
        MountOption::RO,
        MountOption::RW,
        MountOption::Exec,
        MountOption::NoExec,
        MountOption::Atime,
        MountOption::NoAtime,
        MountOption::DirSync,
        MountOption::Sync,
        MountOption::Async,
    ] {
        assert_eq!(
            crate::mnt::mount_options::MountOption::from_str(&option_to_string(&option)),
            option
        );
    }
}

#[hegel::test]
fn mount_option_conflicts_are_order_and_duplicate_independent(tc: TestCase) {
    let count = tc.draw(gs::integers::<usize>().max_value(30));
    let options: Vec<_> = (0..count).map(|_| draw_option(&tc)).collect();
    let mut reversed = options.clone();
    reversed.reverse();
    let mut duplicated = options.clone();
    duplicated.extend(options.iter().cloned());
    let result = |mount_options| {
        check_option_conflicts(&Config {
            mount_options,
            ..Config::default()
        })
        .is_ok()
    };
    assert_eq!(result(options.clone()), result(reversed));
    assert_eq!(result(options.clone()), result(duplicated));
}
