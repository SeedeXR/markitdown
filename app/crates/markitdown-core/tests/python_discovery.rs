//! Auto-discovery of the `markitdown-py` engine binary (so an installed app
//! works without `MARKITDOWN_PY_BIN`). Unix-only: it creates an executable
//! stub. Env-mutating (PATH / MARKITDOWN_PY_BIN are process-global), so all
//! tests here run under one lock in this isolated test binary.
#![cfg(unix)]

use markitdown_core::{python_engine_available, ConvertOptions};
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn guard() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn make_exec(dir: &std::path::Path, name: &str) {
    let p = dir.join(name);
    std::fs::write(&p, "#!/bin/sh\ncat >/dev/null\necho ok\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn discovers_markitdown_py_on_path() {
    let _g = guard();
    let dir = std::env::temp_dir().join(format!("mdpy-disc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    make_exec(&dir, "markitdown-py");

    let old_path = std::env::var_os("PATH");
    let old_bin = std::env::var_os("MARKITDOWN_PY_BIN");
    std::env::remove_var("MARKITDOWN_PY_BIN");
    std::env::set_var("PATH", &dir);

    // No explicit path, no env var -> must be found via PATH discovery.
    let found = python_engine_available(&ConvertOptions::default());

    // restore before asserting
    match old_path {
        Some(p) => std::env::set_var("PATH", p),
        None => std::env::remove_var("PATH"),
    }
    if let Some(b) = old_bin {
        std::env::set_var("MARKITDOWN_PY_BIN", b);
    }
    std::fs::remove_dir_all(&dir).ok();

    assert!(found, "markitdown-py on PATH should be auto-discovered");
}

#[test]
fn explicit_directory_resolves_to_inner_binary() {
    // onedir builds are a FOLDER; pointing python_bin at the folder (a common
    // mistake / what a folder-picker yields) must resolve to the inner
    // `markitdown-py` rather than failing as "not a file".
    let _g = guard();
    let dir = std::env::temp_dir().join(format!("mdpy-onedir-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    make_exec(&dir, "markitdown-py");

    let opts = ConvertOptions {
        engine: markitdown_core::Engine::Python,
        python_bin: Some(dir.clone()), // the FOLDER, not the inner binary
        ..Default::default()
    };
    let found = python_engine_available(&opts);
    std::fs::remove_dir_all(&dir).ok();
    assert!(found, "a python_bin pointing at the onedir folder should resolve to its inner binary");
}

#[test]
fn does_not_discover_bare_markitdown() {
    // A bare `markitdown` (this suite's own Rust CLI) must NOT be picked up —
    // only the unambiguous `markitdown-py` name.
    let _g = guard();
    let dir = std::env::temp_dir().join(format!("mdpy-bare-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    make_exec(&dir, "markitdown"); // NOT markitdown-py

    let old_path = std::env::var_os("PATH");
    let old_bin = std::env::var_os("MARKITDOWN_PY_BIN");
    std::env::remove_var("MARKITDOWN_PY_BIN");
    std::env::set_var("PATH", &dir);

    let found = python_engine_available(&ConvertOptions::default());

    match old_path {
        Some(p) => std::env::set_var("PATH", p),
        None => std::env::remove_var("PATH"),
    }
    if let Some(b) = old_bin {
        std::env::set_var("MARKITDOWN_PY_BIN", b);
    }
    std::fs::remove_dir_all(&dir).ok();

    assert!(!found, "a bare `markitdown` must never be taken as the Python engine");
}

/// Save/restore an environment variable around a test body.
fn with_env<T>(key: &str, value: Option<&std::path::Path>, f: impl FnOnce() -> T) -> T {
    let old = std::env::var_os(key);
    match value {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
    let out = f();
    match old {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
    out
}

#[test]
fn discovers_engine_in_user_install_prefix_without_path() {
    // THE INSTALLED-APP BUG: a GUI app launched from Finder/Dock (and an MCP
    // server spawned by a host) inherits neither MARKITDOWN_PY_BIN nor a useful
    // PATH. An engine installed under ~/.local/share/markitdown must still be
    // found, or the app reports "python engine not configured" forever.
    let _g = guard();
    let home = std::env::temp_dir().join(format!("mdpy-home-{}", std::process::id()));
    let installed = home.join(".local").join("share").join("markitdown");
    std::fs::create_dir_all(&installed).unwrap();
    make_exec(&installed, "markitdown-py");

    let empty = std::env::temp_dir().join(format!("mdpy-nopath-{}", std::process::id()));
    std::fs::create_dir_all(&empty).unwrap();

    let found = with_env("MARKITDOWN_PY_BIN", None, || {
        with_env("PATH", Some(&empty), || {
            with_env("HOME", Some(&home), || {
                python_engine_available(&ConvertOptions::default())
            })
        })
    });

    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&empty).ok();
    assert!(
        found,
        "an engine in ~/.local/share/markitdown must be found without PATH or env"
    );
}

/// End-to-end: run the built CLI from inside a macOS-style `.app` layout with a
/// stripped environment, and confirm it discovers an engine bundled in
/// `Contents/Resources`. This is the exact shape of a DMG install, and it
/// cannot be tested in-process because discovery keys off `current_exe()`.
#[test]
#[cfg(target_os = "macos")]
fn installed_app_bundle_discovers_bundled_engine() {
    let exe = match std::env::var_os("CARGO_BIN_EXE_markitdown").map(std::path::PathBuf::from) {
        Some(p) => p,
        // markitdown-core's tests don't build the CLI; locate it beside the
        // test binary and skip when it hasn't been built.
        None => {
            let mut dir = std::env::current_exe().unwrap();
            dir.pop(); // deps/
            dir.pop();
            let candidate = dir.join("markitdown");
            if !candidate.is_file() {
                eprintln!("skipping: CLI binary not built at {}", candidate.display());
                return;
            }
            candidate
        }
    };

    let root = std::env::temp_dir().join(format!("mdpy-app-{}", std::process::id()));
    let macos = root.join("MarkItDown.app").join("Contents").join("MacOS");
    let resources = root.join("MarkItDown.app").join("Contents").join("Resources");
    std::fs::create_dir_all(&macos).unwrap();
    std::fs::create_dir_all(&resources).unwrap();
    std::fs::copy(&exe, macos.join("markitdown")).unwrap();
    std::fs::set_permissions(
        macos.join("markitdown"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    make_exec(&resources, "markitdown-py");

    let out = std::process::Command::new(macos.join("markitdown"))
        .arg("--check")
        // Reproduce a Finder launch: no inherited shell environment at all.
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("run bundled CLI");
    let report = String::from_utf8_lossy(&out.stdout).to_string();
    std::fs::remove_dir_all(&root).ok();

    assert!(
        !report.contains("python fallback engine : not configured"),
        "an engine in Contents/Resources must be discovered by a bundled app; got:\n{report}"
    );
}
