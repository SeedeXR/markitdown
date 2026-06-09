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
