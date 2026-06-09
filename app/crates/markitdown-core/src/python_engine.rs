//! Optional delegation to a PyInstaller-compiled Python markitdown binary.
//!
//! This is the escape hatch for the long tail the pure-Rust engine does not
//! cover locally (OCR for scanned documents, audio transcription, Azure
//! converters, Python plugins). It is strictly opt-in: nothing here runs
//! unless the caller selects [`crate::options::Engine::Python`]/`Auto` *and*
//! a binary is configured. See `app/python-engine/README.md` for how to build
//! one.

use crate::{ConvertError, ConvertOptions, ConvertResult, StreamInfo};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub const PY_BIN_ENV: &str = "MARKITDOWN_PY_BIN";
/// Override the fallback timeout (seconds). Default: 300.
pub const PY_TIMEOUT_ENV: &str = "MARKITDOWN_PY_TIMEOUT";
/// Extra whitespace-separated args appended to every Python-engine call,
/// e.g. Azure Document Intelligence: `-d -e https://<res>.cognitiveservices.azure.com/`.
pub const PY_ARGS_ENV: &str = "MARKITDOWN_PY_ARGS";

const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

fn timeout() -> std::time::Duration {
    std::env::var(PY_TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(std::time::Duration::from_secs)
        .unwrap_or(DEFAULT_TIMEOUT)
}

/// The unambiguous name of the bundled/installed Python engine binary. We
/// NEVER auto-discover a bare `markitdown` (that's this suite's own Rust CLI —
/// discovering it would make the engine invoke itself). Only this exact name
/// is searched for, so discovery can't pick the wrong binary.
#[cfg(windows)]
const PY_BIN_NAME: &str = "markitdown-py.exe";
#[cfg(not(windows))]
const PY_BIN_NAME: &str = "markitdown-py";

/// Resolve the Python engine binary, trying (in order):
/// 1. an explicit `opts.python_bin` (CLI `--python-bin`, desktop Settings),
/// 2. the `MARKITDOWN_PY_BIN` environment variable,
/// 3. auto-discovery: a `markitdown-py` binary next to the running executable
///    (a Tauri sidecar / side-by-side install) or anywhere on `PATH`.
///
/// Step 3 is what makes an *installed* app "just work" without the user
/// exporting an env var — important because GUI apps (Finder/dock-launched)
/// don't inherit the shell environment on macOS/Linux/Windows.
pub fn resolve_python_bin(opts: &ConvertOptions) -> Option<PathBuf> {
    if let Some(p) = opts.python_bin.clone().and_then(normalize_bin) {
        return Some(p);
    }
    if let Some(p) = std::env::var_os(PY_BIN_ENV)
        .map(PathBuf::from)
        .and_then(normalize_bin)
    {
        return Some(p);
    }
    discover_python_bin()
}

/// Accept either the engine executable directly, or a directory containing it.
/// onedir builds produce a *folder* `markitdown-py/` whose launcher is
/// `markitdown-py/markitdown-py` — users (and file pickers) often point at the
/// folder, so resolve that to the inner binary instead of failing.
fn normalize_bin(p: PathBuf) -> Option<PathBuf> {
    if p.is_file() {
        return Some(p);
    }
    if p.is_dir() {
        let inner = p.join(PY_BIN_NAME);
        if inner.is_file() {
            return Some(inner);
        }
    }
    None
}

/// Look for the engine next to the current executable, then on `PATH`. Both a
/// one-file `markitdown-py` and an onedir **folder** (`markitdown-py/markitdown-py`)
/// are recognized, so dropping either next to the app — or a Tauri sidecar —
/// is auto-discovered.
fn discover_python_bin() -> Option<PathBuf> {
    fn in_dir(dir: &std::path::Path) -> Option<PathBuf> {
        // one-file binary directly in `dir`
        let one = dir.join(PY_BIN_NAME);
        if one.is_file() {
            return Some(one);
        }
        // onedir layout: `dir/markitdown-py/markitdown-py[.exe]`
        let onedir = dir.join("markitdown-py").join(PY_BIN_NAME);
        if onedir.is_file() {
            return Some(onedir);
        }
        None
    }

    // Next to the running binary (side-by-side CLI install / Tauri sidecar).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if let Some(p) = in_dir(dir) {
                return Some(p);
            }
        }
    }
    // Anywhere on PATH (markitdown-py installed system-wide).
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if let Some(p) = in_dir(&dir) {
                return Some(p);
            }
        }
    }
    None
}

/// True when a usable Python fallback binary is configured.
pub fn python_engine_available(opts: &ConvertOptions) -> bool {
    resolve_python_bin(opts).is_some()
}

/// Convert by piping the stream through the Python markitdown binary.
pub fn convert_with_python(
    data: &[u8],
    info: &StreamInfo,
    opts: &ConvertOptions,
) -> Result<ConvertResult, ConvertError> {
    let bin = resolve_python_bin(opts).ok_or_else(|| {
        ConvertError::MissingDependency(format!(
            "python engine requested but no binary found (set {PY_BIN_ENV} or --python-bin; \
             build one with app/python-engine/build_binary.sh)"
        ))
    })?;

    let mut cmd = Command::new(&bin);
    // Enable Python plugins (e.g. markitdown-ocr) when the binary was built
    // with them; harmless otherwise.
    cmd.arg("-p");
    // Extra pass-through args, e.g. Azure Document Intelligence:
    //   MARKITDOWN_PY_ARGS="-d -e https://<resource>.cognitiveservices.azure.com/"
    if let Ok(extra) = std::env::var(PY_ARGS_ENV) {
        cmd.args(extra.split_whitespace());
    }

    // Choose how to hand the input over, in order of fidelity:
    // 1. http(s)/file/data URL as an argument — the Python engine re-fetches
    //    it itself, which keeps its URL-gated converters working (YouTube
    //    transcripts, Wikipedia, Bing SERP).
    // 2. Local path as an argument — zero-copy for large files.
    // 3. Raw bytes over stdin with -x/-m hints — last resort.
    enum Input<'a> {
        Arg(String),
        Stdin(&'a [u8]),
    }
    let input = if let Some(url) = info.url.as_ref().filter(|u| {
        u.starts_with("http:") || u.starts_with("https:") || u.starts_with("data:")
    }) {
        Input::Arg(url.clone())
    } else if let Some(path) = info.local_path.as_ref().filter(|p| p.is_file()) {
        Input::Arg(path.to_string_lossy().into_owned())
    } else {
        Input::Stdin(data)
    };

    if opts.keep_data_uris {
        cmd.arg("--keep-data-uris");
    }
    match &input {
        Input::Arg(src) => {
            cmd.arg(src);
            cmd.stdin(Stdio::null());
        }
        Input::Stdin(_) => {
            if let Some(ext) = &info.extension {
                cmd.arg("-x").arg(ext);
            }
            if let Some(mt) = &info.mimetype {
                cmd.arg("-m").arg(mt);
            }
            cmd.stdin(Stdio::piped());
        }
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| ConvertError::conversion("python-engine", format!("spawn failed: {e}")))?;

    // Feed stdin from a separate thread: writing the input and draining the
    // output concurrently is the only deadlock-free shape when both pipes
    // can fill up.
    let writer = match input {
        Input::Stdin(bytes) => {
            let mut stdin = child.stdin.take().expect("stdin piped");
            let payload = bytes.to_vec();
            Some(std::thread::spawn(move || {
                let _ = stdin.write_all(&payload);
                // stdin drops here, closing the pipe so the child sees EOF.
            }))
        }
        Input::Arg(_) => None,
    };

    // Heartbeat: the Python subprocess is opaque (it streams Markdown to
    // stdout, not progress), so per-page % isn't available — but we can keep
    // the UI/logs alive with an elapsed-time tick every few seconds, mirroring
    // the Rust path's liveness. Only runs when a progress sink is installed.
    let heartbeat_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let heartbeat = opts.progress.clone().map(|cb| {
        let stop = heartbeat_stop.clone();
        std::thread::spawn(move || {
            let start = std::time::Instant::now();
            // Emit one liveness tick immediately, before the first sleep, so a
            // progress event is guaranteed the moment the (opaque) subprocess
            // starts — independent of thread-scheduler timing. Without this a
            // fast subprocess on a loaded runner can finish and set `stop`
            // before the heartbeat thread's first periodic emit, producing zero
            // liveness events (a flaky-test source).
            cb.report(crate::Progress::msg("python", "Python engine running… 0s elapsed"));
            loop {
                // Sleep in short slices so we stop promptly when done.
                for _ in 0..20 {
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                cb.report(crate::Progress::msg(
                    "python",
                    format!("Python engine running… {:.0}s elapsed", start.elapsed().as_secs_f64()),
                ));
            }
        })
    });

    let out = wait_with_timeout(child, timeout())?;
    heartbeat_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(h) = heartbeat {
        let _ = h.join();
    }
    if let Some(w) = writer {
        let _ = w.join();
    }

    if !out.status.success() {
        return Err(ConvertError::conversion(
            "python-engine",
            format!(
                "exit {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    Ok(ConvertResult::new(String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// Drain the child's pipes on a worker thread and enforce a wall-clock
/// timeout; the child is killed when it expires so a hung Python process can
/// never wedge a batch job.
fn wait_with_timeout(
    mut child: std::process::Child,
    limit: std::time::Duration,
) -> Result<std::process::Output, ConvertError> {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();
    // `wait_with_output` reads stdout/stderr to EOF, which only happens when
    // the child exits or is killed — so killing on timeout also unblocks
    // this thread.
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let drain = std::thread::spawn(move || {
        use std::io::Read;
        let mut out = Vec::new();
        let mut stdout = stdout;
        let mut stderr = stderr;
        let t = std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = stderr.read_to_end(&mut v);
            v
        });
        let _ = stdout.read_to_end(&mut out);
        let err = t.join().unwrap_or_default();
        let _ = tx.send((out, err));
    });

    let deadline = std::time::Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    // Do NOT join `drain` here: grandchildren (e.g. a shell's
                    // forked subprocess) may still hold the pipe's write end,
                    // so the reader could stay blocked long after the kill.
                    // The detached thread exits once the pipe finally closes.
                    drop(drain);
                    return Err(ConvertError::conversion(
                        "python-engine",
                        format!("timed out after {}s (set {PY_TIMEOUT_ENV} to adjust)", limit.as_secs()),
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(e) => {
                return Err(ConvertError::conversion(
                    "python-engine",
                    format!("wait failed: {e}"),
                ))
            }
        }
    };
    // Bounded wait for the drained output; same grandchild caveat as above,
    // hence recv_timeout + detach instead of an unbounded join.
    let (stdout, stderr) = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap_or_default();
    drop(drain);
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}
