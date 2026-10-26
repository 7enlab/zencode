



















use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};


const MAX_SYMLINK_HOPS: usize = 40;


pub const TEMP_PREFIX: &str = ".zen-tmp-";

static COUNTER: AtomicU64 = AtomicU64::new(0);




pub fn resolve_write_target(path: &Path) -> io::Result<PathBuf> {
    let mut cur = path.to_path_buf();
    for _ in 0..MAX_SYMLINK_HOPS {
        match fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => {
                let link = fs::read_link(&cur)?;
                cur = if link.is_absolute() {
                    link
                } else {
                    cur.parent().unwrap_or_else(|| Path::new("")).join(link)
                };
            }
            _ => return Ok(cur),
        }
    }
    Err(io::Error::other(format!(
        "too many levels of symbolic links: {}",
        path.display()
    )))
}

fn temp_path(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut file = std::ffi::OsString::from(TEMP_PREFIX);
    file.push(name);
    file.push(format!(".{}.{}", std::process::id(), n));
    dir.join(file)
}


fn create_temp(dir: &Path, name: &std::ffi::OsStr) -> io::Result<(PathBuf, File)> {
    let mut last = None;
    for _ in 0..16 {
        let p = temp_path(dir, name);
        match OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(f) => return Ok((p, f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("could not create a temp file")))
}



pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<Option<String>> {
    let target = resolve_write_target(path)?;
    let name = target
        .file_name()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("not a file path: {}", target.display()),
            )
        })?
        .to_os_string();
    let dir = match target.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let old_perms = match fs::metadata(&target) {
        Ok(m) if m.is_dir() => {
            return Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                format!("{} is a directory", target.display()),
            ))
        }
        Ok(m) => Some(m.permissions()),
        Err(_) => None,
    };

    let (tmp, mut file) = create_temp(&dir, &name)?;
    let staged = (|| {
        file.write_all(data)?;
        if let Some(p) = &old_perms {
            file.set_permissions(p.clone())?;
        }
        file.sync_all()
    })();
    drop(file);
    if let Err(e) = staged {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    match fs::rename(&tmp, &target) {
        Ok(()) => {
            sync_dir(&dir);
            Ok(None)
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            if cfg!(windows) && e.kind() == io::ErrorKind::PermissionDenied {
                fs::write(&target, data)?;
                return Ok(Some(format!(
                    "note: could not replace {} atomically ({}); wrote it in place instead",
                    target.display(),
                    e
                )));
            }
            Err(e)
        }
    }
}


fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}


pub async fn write_atomic_async(path: PathBuf, data: Vec<u8>) -> io::Result<Option<String>> {
    tokio::task::spawn_blocking(move || write_atomic(&path, &data))
        .await
        .map_err(io::Error::other)?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "zen-atomic-{}-{}-{}",
            tag,
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(TEMP_PREFIX))
            .collect()
    }

    #[test]
    fn p0_1_writes_new_and_replaces_existing_without_temp_leftovers() {
        let d = tmpdir("basic");
        let f = d.join("a.txt");
        assert_eq!(write_atomic(&f, b"one\n").unwrap(), None);
        assert_eq!(fs::read(&f).unwrap(), b"one\n");
        write_atomic(&f, b"two, longer than one\n").unwrap();
        assert_eq!(fs::read(&f).unwrap(), b"two, longer than one\n");
        write_atomic(&f, b"").unwrap();
        assert_eq!(fs::read(&f).unwrap(), b"");
        assert!(leftovers(&d).is_empty(), "{:?}", leftovers(&d));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn p0_1_error_leaves_no_temp_and_target_untouched() {
        let d = tmpdir("err");

        let sub = d.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("keep.txt"), "keep").unwrap();
        assert!(write_atomic(&sub, b"x").is_err());
        assert_eq!(fs::read_to_string(sub.join("keep.txt")).unwrap(), "keep");

        assert!(write_atomic(&d.join("missing/x.txt"), b"x").is_err());
        assert!(leftovers(&d).is_empty(), "{:?}", leftovers(&d));
        assert!(leftovers(&sub).is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn p0_1_preserves_mode() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmpdir("mode");
        let f = d.join("run.sh");
        fs::write(&f, "#!/bin/sh\necho old\n").unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o755)).unwrap();
        write_atomic(&f, b"#!/bin/sh\necho new\n").unwrap();
        assert_eq!(
            fs::metadata(&f).unwrap().permissions().mode() & 0o777,
            0o755
        );
        fs::set_permissions(&f, fs::Permissions::from_mode(0o600)).unwrap();
        write_atomic(&f, b"secret\n").unwrap();
        assert_eq!(
            fs::metadata(&f).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn p0_1_symlink_is_written_through_not_replaced() {
        let d = tmpdir("link");
        let real = d.join("real.txt");
        let link = d.join("link.txt");
        fs::write(&real, "old").unwrap();
        std::os::unix::fs::symlink("real.txt", &link).unwrap();
        write_atomic(&link, b"new").unwrap();
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_link(&link).unwrap(), PathBuf::from("real.txt"));
        assert_eq!(fs::read_to_string(&real).unwrap(), "new");

        let link2 = d.join("link2.txt");
        std::os::unix::fs::symlink(&link, &link2).unwrap();
        write_atomic(&link2, b"newer").unwrap();
        assert_eq!(fs::read_to_string(&real).unwrap(), "newer");
        let dangling = d.join("dangling.txt");
        std::os::unix::fs::symlink("born.txt", &dangling).unwrap();
        write_atomic(&dangling, b"born").unwrap();
        assert!(fs::symlink_metadata(&dangling)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_to_string(d.join("born.txt")).unwrap(), "born");
        assert!(leftovers(&d).is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn p0_1_read_only_dir_errors_and_keeps_old_file() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmpdir("ro");
        let f = d.join("a.txt");
        fs::write(&f, "old").unwrap();
        fs::set_permissions(&d, fs::Permissions::from_mode(0o555)).unwrap();

        let probe = d.join("probe");
        let root = fs::write(&probe, "").is_ok();
        let res = write_atomic(&f, b"new");
        fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
        if !root {
            assert!(res.is_err(), "write into a read-only dir must fail");
            assert_eq!(fs::read_to_string(&f).unwrap(), "old");
            assert!(leftovers(&d).is_empty());
        }
        let _ = fs::remove_dir_all(&d);
    }
}
