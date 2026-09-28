//! Compile the Tailwind/DaisyUI stylesheet as part of the crate build.
//!
//! Release builds always (re)generate minified CSS so the shipped binary is
//! self-contained — no separate `just css` step at deploy time. Debug builds
//! regenerate when the existing CSS is stale (older than any source) or missing,
//! so a bare `cargo run` self-heals after a `git pull` — `just dev`'s watcher
//! catches live edits but can miss bulk pulls. A fresh build is a no-op.
//!
//! Requires the standalone `tailwindcss` CLI on PATH (provided by the Nix
//! devshell as `tailwindcss_4`), or an explicit `TAILWINDCSS` override.

use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let input = format!("{manifest}/styles/input.css");
    let output = format!("{manifest}/assets/static/css/app.css");

    // Rebuild when the CSS source or any embedded asset changes.
    println!("cargo:rerun-if-changed=styles");
    println!("cargo:rerun-if-changed=templates");
    println!("cargo:rerun-if-changed=assets");
    println!("cargo:rerun-if-env-changed=TAILWINDCSS");

    let is_release = std::env::var("PROFILE").as_deref() == Ok("release");
    if !is_release && css_is_fresh(&output, &manifest) {
        // Debug build whose CSS is already up to date — nothing to do. (`just
        // dev`'s watcher handles live edits; this catches stale CSS after a pull.)
        return;
    }

    let Some(tailwind) = find_tailwind() else {
        let msg = "tailwindcss not found on PATH (Nix devshell provides tailwindcss_4; \
                   or set TAILWINDCSS, or run `just css`)";
        if is_release {
            panic!("release build needs CSS but {msg}");
        }
        println!("cargo:warning=serving without compiled CSS: {msg}");
        return;
    };

    let mut cmd = Command::new(&tailwind);
    cmd.args(["-i", &input, "-o", &output]);
    if is_release {
        cmd.arg("--minify");
    }
    let status = cmd.status().expect("failed to run tailwindcss");
    assert!(status.success(), "tailwindcss exited with {status}");
}

/// True when the generated `output` exists and is at least as new as every
/// Tailwind input (`styles/` plus the scanned templates in `templates/`).
/// A missing or unreadable output counts as stale.
fn css_is_fresh(output: &str, manifest: &str) -> bool {
    let Ok(out_mtime) = std::fs::metadata(output).and_then(|m| m.modified()) else {
        return false;
    };
    [
        format!("{manifest}/styles"),
        format!("{manifest}/templates"),
    ]
    .iter()
    .filter_map(|dir| newest_mtime(Path::new(dir)))
    .max()
    .is_none_or(|newest_src| out_mtime >= newest_src)
}

/// Newest modification time of any file under `dir` (recursively), or `None` if
/// the directory is empty or unreadable.
fn newest_mtime(dir: &Path) -> Option<SystemTime> {
    let mut newest: Option<SystemTime> = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
                newest = Some(newest.map_or(modified, |n| n.max(modified)));
            }
        }
    }
    newest
}

/// Resolve the Tailwind CLI: explicit `TAILWINDCSS` override, else look on PATH.
fn find_tailwind() -> Option<String> {
    if let Ok(path) = std::env::var("TAILWINDCSS") {
        return Some(path);
    }
    let finder = if cfg!(windows) { "where" } else { "which" };
    let out = Command::new(finder).arg("tailwindcss").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .trim()
        .to_string();
    (!path.is_empty()).then_some(path)
}
