//! Build NFL-data's independently owned admin stylesheet.
use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let input = format!("{manifest}/styles/input.css");
    let output = format!("{manifest}/assets/static/css/admin.css");
    println!("cargo:rerun-if-changed=styles");
    println!("cargo:rerun-if-changed=templates");
    println!("cargo:rerun-if-changed=assets");
    println!("cargo:rerun-if-env-changed=TAILWINDCSS");

    let release = std::env::var("PROFILE").as_deref() == Ok("release");
    if !release && css_is_fresh(&output, &manifest) {
        return;
    }
    let Some(tailwind) = find_tailwind() else {
        let message = "tailwindcss not found on PATH (or set TAILWINDCSS)";
        if release {
            panic!("release build needs CSS but {message}");
        }
        println!("cargo:warning=serving without compiled NFL admin CSS: {message}");
        return;
    };
    std::fs::create_dir_all(Path::new(&output).parent().unwrap()).unwrap();
    let mut command = Command::new(tailwind);
    command.args(["-i", &input, "-o", &output]);
    if release {
        command.arg("--minify");
    }
    let status = command.status().expect("failed to run tailwindcss");
    assert!(status.success(), "tailwindcss exited with {status}");
}

fn css_is_fresh(output: &str, manifest: &str) -> bool {
    let Ok(output_time) = std::fs::metadata(output).and_then(|metadata| metadata.modified()) else {
        return false;
    };
    [
        format!("{manifest}/styles"),
        format!("{manifest}/templates"),
    ]
    .iter()
    .filter_map(|directory| newest_mtime(Path::new(directory)))
    .max()
    .is_none_or(|newest| output_time >= newest)
}

fn newest_mtime(directory: &Path) -> Option<SystemTime> {
    let mut newest = None;
    let mut pending = vec![directory.to_path_buf()];
    while let Some(path) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) {
                newest = Some(newest.map_or(modified, |current: SystemTime| current.max(modified)));
            }
        }
    }
    newest
}

fn find_tailwind() -> Option<String> {
    if let Ok(path) = std::env::var("TAILWINDCSS") {
        return Some(path);
    }
    let finder = if cfg!(windows) { "where" } else { "which" };
    let output = Command::new(finder).arg("tailwindcss").output().ok()?;
    if !output.status.success() {
        return None;
    }
    output
        .stdout
        .split(|byte| *byte == b'\n')
        .next()
        .map(|line| String::from_utf8_lossy(line).trim().to_owned())
        .filter(|path| !path.is_empty())
}
