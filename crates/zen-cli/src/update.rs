use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const DEFAULT_REPO: &str = "https://github.com/7enlabs/zencode.git";
pub const DEFAULT_BRANCH: &str = "main";

pub fn default_src_dir(home: Option<&Path>) -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("ZENCODE_SRC_DIR") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    home.map(|h| h.join(".local").join("share").join("zencode"))
}

fn repo_url() -> String {
    std::env::var("ZENCODE_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string())
}

fn find_cargo(home: Option<&Path>) -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in ["cargo", "cargo.exe"] {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    if let Some(h) = home {
        for name in ["cargo", "cargo.exe"] {
            let candidate = h.join(".cargo").join("bin").join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn run(program: &Path, args: &[&str], cwd: Option<&Path>) -> Result<()> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let status = cmd
        .status()
        .with_context(|| format!("failed to run {}", program.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "command failed: {} {}",
            program.display(),
            args.join(" ")
        ))
    }
}

fn run_output(program: &str, args: &[&str], cwd: &Path) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run {program}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(anyhow!(
            "{} failed: {}",
            program,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn parse_ls_remote_head(out: &str) -> Option<String> {
    let first = out.lines().next()?;
    let sha = first.split_whitespace().next()?;
    if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(sha.to_string())
    } else {
        None
    }
}

fn backup_path_for(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_os_string();
    name.push(".old");
    target.with_file_name(name)
}

fn built_binary_name() -> &'static str {
    if cfg!(windows) {
        "zen.exe"
    } else {
        "zen"
    }
}

pub fn run_update(check: bool, home: Option<&Path>) -> Result<i32> {
    let Some(src_dir) = default_src_dir(home) else {
        anyhow::bail!("cannot determine home directory; set ZENCODE_SRC_DIR");
    };
    let git = "git";
    if Command::new(git).arg("--version").output().is_err() {
        anyhow::bail!("git not found; install Git first, then rerun zen update");
    }

    if !src_dir.join(".git").is_dir() {
        if check {
            println!("no local checkout found at {}", src_dir.display());
            println!("run zen update without --check to install from source");
            return Ok(0);
        }
        println!(
            "Cloning {} ({}) into {}",
            repo_url(),
            DEFAULT_BRANCH,
            src_dir.display()
        );
        if let Some(parent) = src_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        run(
            Path::new(git),
            &[
                "clone",
                "--branch",
                DEFAULT_BRANCH,
                "--depth",
                "1",
                &repo_url(),
                src_dir
                    .to_str()
                    .ok_or_else(|| anyhow!("invalid checkout path"))?,
            ],
            None,
        )?;
    } else {
        let status = run_output(git, &["status", "--porcelain"], &src_dir)?;
        if !status.trim().is_empty() {
            anyhow::bail!(
                "local checkout at {} has uncommitted changes; commit or stash them first",
                src_dir.display()
            );
        }
        let local = run_output(git, &["rev-parse", "HEAD"], &src_dir)?;
        let local = local.trim().to_string();
        let remote_out = run_output(git, &["ls-remote", "origin", "HEAD"], &src_dir)?;
        let remote = parse_ls_remote_head(&remote_out)
            .ok_or_else(|| anyhow!("could not read remote HEAD"))?;
        if check {
            if local == remote {
                println!("up to date: {local}");
            } else {
                println!("update available: local {local}, remote {remote}");
            }
            return Ok(0);
        }
        if local == remote {
            println!("already up to date: {local}");
            return Ok(0);
        }
        println!("Updating checkout from {local} to {remote}");
        run(
            Path::new(git),
            &["fetch", "origin", DEFAULT_BRANCH],
            Some(&src_dir),
        )?;
        run(
            Path::new(git),
            &["checkout", DEFAULT_BRANCH],
            Some(&src_dir),
        )?;
        run(
            Path::new(git),
            &["merge", "--ff-only", &format!("origin/{DEFAULT_BRANCH}")],
            Some(&src_dir),
        )?;
    }

    let Some(cargo) = find_cargo(home) else {
        anyhow::bail!("cargo not found; install Rust first, for example from https://rustup.rs");
    };
    println!("Building zen-cli in release mode (this can take a few minutes)");
    run(
        &cargo,
        &[
            "build",
            "--release",
            "-p",
            "zen-cli",
            "--manifest-path",
            src_dir
                .join("Cargo.toml")
                .to_str()
                .ok_or_else(|| anyhow!("invalid manifest path"))?,
        ],
        None,
    )?;

    let built = src_dir
        .join("target")
        .join("release")
        .join(built_binary_name());
    if !built.is_file() {
        anyhow::bail!("build finished but {} is missing", built.display());
    }
    let current = std::env::current_exe()?;
    let target = current.with_file_name(built_binary_name());
    if built.canonicalize().ok() == target.canonicalize().ok() {
        println!("already running the freshly built binary");
        return Ok(0);
    }
    let backup = backup_path_for(&target);
    if backup.exists() {
        let _ = std::fs::remove_file(&backup);
    }
    if target.exists() {
        std::fs::rename(&target, &backup).with_context(|| {
            "could not move the running binary aside; close other zen processes and retry"
        })?;
    }
    if let Err(e) = std::fs::copy(&built, &target) {
        let _ = std::fs::rename(&backup, &target);
        return Err(e).context("could not install the new binary; the old binary was restored");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
    }
    let head = run_output(git, &["rev-parse", "--short", "HEAD"], &src_dir)?;
    println!("Updated zen to {} at {}", head.trim(), target.display());
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_src_dir_uses_home() {
        let dir = default_src_dir(Some(Path::new("/home/test")));
        assert_eq!(dir, Some(PathBuf::from("/home/test/.local/share/zencode")));
    }

    #[test]
    fn parse_ls_remote_head_reads_first_field() {
        assert_eq!(
            parse_ls_remote_head("0123456789abcdef0123456789abcdef01234567\tHEAD\n"),
            Some("0123456789abcdef0123456789abcdef01234567".to_string())
        );
        assert_eq!(parse_ls_remote_head(""), None);
        assert_eq!(parse_ls_remote_head("not-a-sha\tHEAD\n"), None);
    }

    #[test]
    fn backup_path_appends_old() {
        assert_eq!(
            backup_path_for(Path::new("/tmp/bin/zen")),
            PathBuf::from("/tmp/bin/zen.old")
        );
        assert_eq!(
            backup_path_for(Path::new("/tmp/bin/zen.exe")),
            PathBuf::from("/tmp/bin/zen.exe.old")
        );
    }
}
