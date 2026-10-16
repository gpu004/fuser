#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ffi::CString;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use hegel::TestCase;
use hegel::generators as gs;

const SNAPSHOT_EXCLUSIONS: &[(&str, &str)] = &[
    (
        "inode number",
        "The mounted and reference filesystems allocate different inodes.",
    ),
    (
        "directory st_size",
        "Directory sizes are filesystem implementation details.",
    ),
    (
        "st_blocks",
        "Block allocation differs between the backing and mounted filesystems.",
    ),
    (
        "atime",
        "Reading and traversing a tree updates access times independently.",
    ),
    (
        "ctime",
        "Metadata change timestamps include filesystem-specific update timing.",
    ),
    (
        "directory nlink",
        "https://github.com/gpu004/fuser/issues/1",
    ),
];

const OPERATION_EXCLUSIONS: &[(&str, &str)] = &[
    (
        "SEEK_DATA on a nonempty hole-only file: ext4 ENXIO versus simple offset 0",
        "not a bug: simple has no lseek, kernel generic fallback treats file as all data",
    ),
    (
        "SEEK_HOLE on a nonempty hole-only file: ext4 offset 0 versus simple EOF",
        "not a bug: simple has no lseek, kernel generic fallback treats file as all data",
    ),
];

#[derive(Debug, Eq, PartialEq)]
struct Node {
    kind: u32,
    mode: u32,
    nlink: Option<u64>,
    size: Option<u64>,
    contents: Vec<u8>,
    target: Vec<u8>,
    xattrs: BTreeMap<Vec<u8>, Vec<u8>>,
    mtime: Option<(i64, i64)>,
}

#[derive(Debug, Eq, PartialEq)]
struct MountSnapshot(BTreeMap<PathBuf, Node>);

struct MountGuard {
    child: Child,
    mount_point: PathBuf,
}

impl MountGuard {
    fn start(data_dir: &Path, mount_point: &Path) -> Self {
        let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("debug")
            .join("examples")
            .join("simple");
        assert!(
            binary.is_file(),
            "missing {}: run cargo +stable build --example simple first",
            binary.display()
        );
        let log_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("hegel_simple_differential.log");
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .unwrap();
        let mut child = Command::new(binary)
            .arg("--data-dir")
            .arg(data_dir)
            .arg("--mount-point")
            .arg(mount_point)
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        for _ in 0..200 {
            if Command::new("mountpoint")
                .arg("-q")
                .arg(mount_point)
                .status()
                .is_ok_and(|status| status.success())
            {
                return Self {
                    child,
                    mount_point: mount_point.to_owned(),
                };
            }
            if child.try_wait().unwrap().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = Command::new("fusermount3")
            .arg("-u")
            .arg(mount_point)
            .status();
        let _ = child.kill();
        let _ = child.wait();
        panic!(
            "simple did not mount at {}; see target/hegel_simple_differential.log",
            mount_point.display()
        );
    }
}

impl Drop for MountGuard {
    fn drop(&mut self) {
        let _ = Command::new("fusermount3")
            .arg("-u")
            .arg(&self.mount_point)
            .status();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn compare_results<T: Eq + std::fmt::Debug>(
    operation: &str,
    reference: io::Result<T>,
    mounted: io::Result<T>,
) {
    match (reference, mounted) {
        (Ok(left), Ok(right)) => assert_eq!(left, right, "{operation}: return values differ"),
        (Err(left), Err(right)) => assert_eq!(
            left.raw_os_error(),
            right.raw_os_error(),
            "{operation}: errno differs: {left:?} versus {right:?}"
        ),
        (left, right) => panic!("{operation}: result kinds differ: {left:?} versus {right:?}"),
    }
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

fn errno_result(value: libc::c_int) -> io::Result<()> {
    if value == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn set_xattr(path: &Path, name: &[u8], value: &[u8]) -> io::Result<()> {
    let path = c_path(path)?;
    let name = CString::new(name).unwrap();
    let result = unsafe {
        libc::lsetxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    errno_result(result)
}

fn get_xattr(path: &Path, name: &[u8]) -> io::Result<Vec<u8>> {
    let path = c_path(path)?;
    let name = CString::new(name).unwrap();
    let size = unsafe { libc::lgetxattr(path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
    if size < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut value = vec![0; usize::try_from(size).unwrap()];
    let result = unsafe {
        libc::lgetxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        value.truncate(usize::try_from(result).unwrap());
        Ok(value)
    }
}

fn list_xattrs(path: &Path) -> io::Result<BTreeMap<Vec<u8>, Vec<u8>>> {
    let path_c = c_path(path)?;
    let size = unsafe { libc::llistxattr(path_c.as_ptr(), std::ptr::null_mut(), 0) };
    if size < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut names = vec![0; usize::try_from(size).unwrap()];
    let size = unsafe { libc::llistxattr(path_c.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
    if size < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut result = BTreeMap::new();
    for name in names[..usize::try_from(size).unwrap()]
        .split(|byte| *byte == 0)
        .filter(|name| name.starts_with(b"user."))
        .filter(|name| !name.is_empty())
    {
        result.insert(name.to_vec(), get_xattr(path, name)?);
    }
    Ok(result)
}

fn set_times(path: &Path, seconds: i64, nanos: i64) -> io::Result<()> {
    let path = c_path(path)?;
    let times = [
        libc::timespec {
            tv_sec: seconds,
            tv_nsec: nanos,
        },
        libc::timespec {
            tv_sec: seconds,
            tv_nsec: nanos,
        },
    ];
    let result = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            path.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    errno_result(result)
}

fn fallocate(path: &Path, offset: i64, length: i64) -> io::Result<i64> {
    let file = OpenOptions::new().write(true).open(path)?;
    let result = unsafe { libc::fallocate(file.as_raw_fd(), 0, offset, length) };
    if result == 0 {
        Ok(0)
    } else {
        Err(io::Error::last_os_error())
    }
}

fn copy_file_range(source: &Path, destination: &Path, length: usize) -> io::Result<usize> {
    let source = File::open(source)?;
    let destination = OpenOptions::new().write(true).open(destination)?;
    let result = unsafe {
        libc::copy_file_range(
            source.as_raw_fd(),
            std::ptr::null_mut(),
            destination.as_raw_fd(),
            std::ptr::null_mut(),
            length,
            0,
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(usize::try_from(result).unwrap())
    }
}

fn seek_data_or_hole(path: &Path, whence: libc::c_int) -> io::Result<i64> {
    let file = File::open(path)?;
    let result = unsafe { libc::lseek(file.as_raw_fd(), 0, whence) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

fn compare_seek_data_results(
    reference: io::Result<i64>,
    mounted: io::Result<i64>,
    reference_path: &Path,
) {
    let hole_only_file = hole_only_file_size(reference_path).is_some();
    let excluded = hole_only_file
        && matches!((&reference, &mounted), (Err(error), Ok(0)) if error.raw_os_error() == Some(libc::ENXIO));
    if excluded {
        return;
    }
    compare_results("lseek SEEK_DATA", reference, mounted);
}

fn compare_seek_hole_results(
    reference: io::Result<i64>,
    mounted: io::Result<i64>,
    reference_path: &Path,
) {
    let size = hole_only_file_size(reference_path);
    let excluded = matches!((&reference, &mounted), (Ok(0), Ok(offset))
        if size.is_some_and(|size| i64::try_from(size).ok() == Some(*offset)));
    if excluded {
        return;
    }
    compare_results("lseek SEEK_HOLE", reference, mounted);
}

fn hole_only_file_size(path: &Path) -> Option<u64> {
    let metadata = fs::metadata(path).ok()?;
    (metadata.size() > 0 && metadata.blocks() == 0).then_some(metadata.size())
}

fn clear_explicit_mtime_for_modified_path(
    explicit_mtimes: &mut BTreeSet<PathBuf>,
    reference_root: &Path,
    relative_path: &Path,
) {
    let Ok(modified) = fs::metadata(reference_root.join(relative_path)) else {
        return;
    };
    explicit_mtimes.retain(|path| {
        fs::symlink_metadata(reference_root.join(path)).is_ok_and(|metadata| {
            metadata.dev() != modified.dev() || metadata.ino() != modified.ino()
        })
    });
}

fn snapshot(root: &Path, explicit_mtimes: &BTreeSet<PathBuf>) -> io::Result<MountSnapshot> {
    fn visit(
        root: &Path,
        path: &Path,
        explicit_mtimes: &BTreeSet<PathBuf>,
        output: &mut BTreeMap<PathBuf, Node>,
    ) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        let relative = path.strip_prefix(root).unwrap_or(Path::new(""));
        let kind = metadata.mode() & libc::S_IFMT;
        let is_dir = kind == libc::S_IFDIR;
        let is_link = kind == libc::S_IFLNK;
        let relative = if relative.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            relative.to_owned()
        };
        let contents = if is_dir || is_link {
            Vec::new()
        } else {
            fs::read(path)?
        };
        let target = if is_link {
            fs::read_link(path)?.as_os_str().as_bytes().to_vec()
        } else {
            Vec::new()
        };
        let mtime = if explicit_mtimes.contains(&relative) {
            Some((metadata.mtime(), metadata.mtime_nsec()))
        } else {
            None
        };
        output.insert(
            relative.clone(),
            Node {
                kind,
                mode: metadata.mode() & 0o7777,
                nlink: (!is_dir).then_some(metadata.nlink()),
                size: (!is_dir).then_some(metadata.size()),
                contents,
                target,
                xattrs: list_xattrs(path)?,
                mtime,
            },
        );
        if is_dir {
            let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
            entries.sort_by_key(|entry| entry.file_name().as_bytes().to_vec());
            for entry in entries {
                visit(root, &entry.path(), explicit_mtimes, output)?;
            }
        }
        Ok(())
    }

    let mut output = BTreeMap::new();
    visit(root, root, explicit_mtimes, &mut output)?;
    Ok(MountSnapshot(output))
}

fn relative_name(index: usize) -> &'static [u8] {
    [b"a".as_slice(), b"b", b"c", b"dir", b"link"][index % 5]
}

fn apply_operation(
    operation: u8,
    reference_root: &Path,
    mounted_root: &Path,
    explicit_mtimes: &mut BTreeSet<PathBuf>,
) {
    let name = relative_name(usize::from(operation));
    let reference = reference_root.join(Path::new(std::str::from_utf8(name).unwrap()));
    let mounted = mounted_root.join(Path::new(std::str::from_utf8(name).unwrap()));
    match operation % 19 {
        0 => {
            let result = |path: &Path| {
                OpenOptions::new()
                    .write(true)
                    .read(true)
                    .create_new(true)
                    .open(path)
                    .map(|_| ())
            };
            let left = result(&reference);
            let right = result(&mounted);
            let succeeded = left.is_ok() && right.is_ok();
            compare_results("create", left, right);
            if succeeded {
                explicit_mtimes.remove(Path::new(std::str::from_utf8(name).unwrap()));
            }
        }
        1 => {
            let data = [operation; 7];
            let offset = u64::from(operation) * 3;
            let left = File::open(&reference).and_then(|file| file.write_at(&data, offset));
            let right = File::open(&mounted).and_then(|file| file.write_at(&data, offset));
            let succeeded = left.is_ok() && right.is_ok();
            compare_results("pwrite", left, right);
            if succeeded {
                clear_explicit_mtime_for_modified_path(
                    explicit_mtimes,
                    reference_root,
                    Path::new(std::str::from_utf8(name).unwrap()),
                );
            }
        }
        2 => {
            let mut left = vec![0; 64];
            let mut right = vec![0; 64];
            let left_result = File::open(&reference).and_then(|file| file.read_at(&mut left, 0));
            let right_result = File::open(&mounted).and_then(|file| file.read_at(&mut right, 0));
            compare_results("pread", left_result, right_result);
            assert_eq!(left, right, "pread: contents differ");
        }
        3 => {
            let length = u64::from(operation % 32);
            let left = OpenOptions::new()
                .write(true)
                .open(&reference)
                .and_then(|file| file.set_len(length));
            let right = OpenOptions::new()
                .write(true)
                .open(&mounted)
                .and_then(|file| file.set_len(length));
            let succeeded = left.is_ok() && right.is_ok();
            compare_results("truncate", left, right);
            if succeeded {
                clear_explicit_mtime_for_modified_path(
                    explicit_mtimes,
                    reference_root,
                    Path::new(std::str::from_utf8(name).unwrap()),
                );
            }
        }
        4..=6 => {
            let (operation_name, left, right) = match operation % 19 {
                4 => (
                    "mkdir",
                    fs::create_dir(&reference),
                    fs::create_dir(&mounted),
                ),
                5 => (
                    "rmdir",
                    fs::remove_dir(&reference),
                    fs::remove_dir(&mounted),
                ),
                _ => (
                    "unlink",
                    fs::remove_file(&reference),
                    fs::remove_file(&mounted),
                ),
            };
            let succeeded = left.is_ok() && right.is_ok();
            compare_results(operation_name, left, right);
            if succeeded {
                explicit_mtimes.remove(Path::new(std::str::from_utf8(name).unwrap()));
            }
        }
        7 => {
            let source_reference = reference_root.join("a");
            let source_mounted = mounted_root.join("a");
            let destination_reference = reference_root.join("b");
            let destination_mounted = mounted_root.join("b");
            let source_key = PathBuf::from("a");
            let destination_key = PathBuf::from("b");
            let source_has_explicit_mtime = explicit_mtimes.remove(&source_key);
            let left = fs::rename(source_reference, destination_reference);
            let right = fs::rename(source_mounted, destination_mounted);
            let succeeded = left.is_ok() && right.is_ok();
            compare_results("rename", left, right);
            if succeeded {
                explicit_mtimes.remove(&destination_key);
                if source_has_explicit_mtime {
                    explicit_mtimes.insert(destination_key);
                }
            } else if source_has_explicit_mtime {
                explicit_mtimes.insert(source_key);
            }
        }
        8 => {
            let source_reference = reference_root.join("a");
            let source_mounted = mounted_root.join("a");
            let destination_reference = reference_root.join("hard");
            let destination_mounted = mounted_root.join("hard");
            compare_results(
                "link",
                fs::hard_link(source_reference, destination_reference),
                fs::hard_link(source_mounted, destination_mounted),
            );
        }
        9 => {
            let target = Path::new("a");
            let left = std::os::unix::fs::symlink(target, &reference);
            let right = std::os::unix::fs::symlink(target, &mounted);
            let succeeded = left.is_ok() && right.is_ok();
            compare_results("symlink", left, right);
            if succeeded {
                explicit_mtimes.remove(Path::new(std::str::from_utf8(name).unwrap()));
            }
        }
        10 => {
            compare_results(
                "readlink",
                fs::read_link(&reference),
                fs::read_link(&mounted),
            );
        }
        11 => {
            let mode = 0o600 | (u32::from(operation) & 0o77);
            compare_results(
                "chmod",
                fs::set_permissions(&reference, fs::Permissions::from_mode(mode)),
                fs::set_permissions(&mounted, fs::Permissions::from_mode(mode)),
            );
        }
        12 => {
            let seconds = i64::from(operation) - 100;
            let result_reference = set_times(&reference, seconds, 123_000_000);
            let result_mounted = set_times(&mounted, seconds, 123_000_000);
            let both_succeeded = result_reference.is_ok() && result_mounted.is_ok();
            compare_results("utimensat", result_reference, result_mounted);
            if both_succeeded {
                explicit_mtimes.insert(PathBuf::from(std::str::from_utf8(name).unwrap()));
            }
        }
        13 => {
            let key = b"user.hegel";
            let value = [operation, operation.wrapping_add(1)];
            compare_results(
                "setxattr",
                set_xattr(&reference, key, &value),
                set_xattr(&mounted, key, &value),
            );
        }
        14 => {
            let key = b"user.hegel";
            compare_results(
                "getxattr",
                get_xattr(&reference, key),
                get_xattr(&mounted, key),
            );
        }
        15 => compare_results("listxattr", list_xattrs(&reference), list_xattrs(&mounted)),
        16 => {
            let key = CString::new("user.hegel").unwrap();
            let reference_path = c_path(&reference).unwrap();
            let mounted_path = c_path(&mounted).unwrap();
            let left = unsafe { libc::lremovexattr(reference_path.as_ptr(), key.as_ptr()) };
            let right = unsafe { libc::lremovexattr(mounted_path.as_ptr(), key.as_ptr()) };
            compare_results("removexattr", errno_result(left), errno_result(right));
        }
        17 => {
            let left = fallocate(&reference, 0, 32);
            let right = fallocate(&mounted, 0, 32);
            let succeeded = left.is_ok() && right.is_ok();
            compare_results("fallocate", left, right);
            if succeeded {
                clear_explicit_mtime_for_modified_path(
                    explicit_mtimes,
                    reference_root,
                    Path::new(std::str::from_utf8(name).unwrap()),
                );
            }
        }
        _ => {
            let source_reference = reference_root.join("a");
            let source_mounted = mounted_root.join("a");
            let destination_reference = reference_root.join("b");
            let destination_mounted = mounted_root.join("b");
            let left = copy_file_range(&source_reference, &destination_reference, 32);
            let right = copy_file_range(&source_mounted, &destination_mounted, 32);
            let changed =
                matches!((&left, &right), (Ok(left), Ok(right)) if *left > 0 && *right > 0);
            compare_results("copy_file_range", left, right);
            if changed {
                clear_explicit_mtime_for_modified_path(
                    explicit_mtimes,
                    reference_root,
                    Path::new("b"),
                );
            }
            if fs::metadata(&source_reference).is_ok_and(|metadata| metadata.is_file()) {
                let left = seek_data_or_hole(&source_reference, libc::SEEK_DATA);
                let right = seek_data_or_hole(&source_mounted, libc::SEEK_DATA);
                compare_seek_data_results(left, right, &source_reference);
                let left = seek_data_or_hole(&source_reference, libc::SEEK_HOLE);
                let right = seek_data_or_hole(&source_mounted, libc::SEEK_HOLE);
                compare_seek_hole_results(left, right, &source_reference);
            }
        }
    }
}

#[hegel::test(test_cases = 200)]
#[ignore]
fn simple_filesystem_differential(tc: TestCase) {
    let operations = tc.draw(gs::vecs(gs::integers::<u8>()).min_size(1).max_size(40));
    let data_dir = tempfile::tempdir().unwrap();
    let mount_dir = tempfile::tempdir().unwrap();
    let reference_dir = tempfile::tempdir().unwrap();
    let guard = MountGuard::start(data_dir.path(), mount_dir.path());
    let case_name = format!("case-{}", operations.len());
    let mounted_case = mount_dir.path().join(&case_name);
    let reference_case = reference_dir.path().join(&case_name);
    fs::create_dir(&mounted_case).unwrap();
    fs::create_dir(&reference_case).unwrap();
    let mut explicit_mtimes = BTreeSet::new();
    for operation in operations {
        apply_operation(
            operation,
            &reference_case,
            &mounted_case,
            &mut explicit_mtimes,
        );
        assert_eq!(
            snapshot(&reference_case, &explicit_mtimes).unwrap(),
            snapshot(&mounted_case, &explicit_mtimes).unwrap(),
            "snapshot exclusions: {SNAPSHOT_EXCLUSIONS:?}; operation exclusions: {OPERATION_EXCLUSIONS:?}"
        );
    }
    drop(guard);
}
