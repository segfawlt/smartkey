use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Find the enclosing git repository root (handles both standard checkouts and
/// linked git worktrees where `.git` is a file pointing at the real gitdir).
fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start.to_path_buf());
    while let Some(d) = dir {
        if d.join(".git").exists() {
            return Some(d);
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    None
}

/// Resolves the actual `.git` directory for both normal checkouts and linked worktrees.
fn resolve_git_dir(repo_root: &Path) -> Option<PathBuf> {
    let dot_git = repo_root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    if dot_git.is_file() {
        let raw = fs::read_to_string(&dot_git).ok()?;
        let rel = raw.trim().strip_prefix("gitdir:")?.trim();
        let p = PathBuf::from(rel);
        return Some(if p.is_absolute() {
            p
        } else {
            repo_root.join(p)
        });
    }
    None
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = find_git_root(&manifest);

    let desc = root
        .as_ref()
        .and_then(|r| {
            Command::new("git")
                .args(["describe", "--always", "--tags", "--dirty=-dirty"])
                .current_dir(r)
                .output()
                .ok()
        })
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "release".to_string());

    println!("cargo:rustc-env=SMARTKEY_GIT_VERSION={desc}");

    if let Some(git_dir) = root.as_deref().and_then(resolve_git_dir) {
        for rel in ["HEAD", "index", "logs/HEAD", "packed-refs"] {
            let p = git_dir.join(rel);
            if p.exists() {
                println!("cargo:rerun-if-changed={}", p.display());
            }
        }
        let commondir_file = git_dir.join("commondir");
        if let Ok(raw) = fs::read_to_string(&commondir_file) {
            let common = git_dir.join(raw.trim());
            for rel in ["packed-refs", "logs/HEAD"] {
                let p = common.join(rel);
                if p.exists() {
                    println!("cargo:rerun-if-changed={}", p.display());
                }
            }
        }
    }
}
