//! Web Playground: build the active Mighty file to WebAssembly and run it in the
//! browser. Mighty targets the WASM Component Model by default and ships web
//! tooling, so this is on-brand.
//!
//! Two modes, picked by [`WebPlayground::start`] from the active file's package:
//!
//! * **serve** — when the file lives in a `mighty.toml` package that has a
//!   `web/` dir (the `mty new --template web-game` shape), spawn
//!   `mty serve --port <p> --manifest-dir <pkg>`. `mty serve` reads
//!   `mighty.toml`, builds with `--target wasm32-web`, and serves `web/` + the
//!   freshly-built `main.wasm` on `127.0.0.1:<port>`, printing
//!   `mty serve: listening on http://127.0.0.1:<port>` — which we scrape to
//!   open the browser.
//! * **build fallback** — otherwise (a bare `.mty` file, or a package with no
//!   `web/`), run `mty build --target wasm32-web <file>` to a `.wasm`, write a
//!   minimal HTML harness that instantiates the component's core module next to
//!   it, and serve that dir statically (Python `http.server`), opening the
//!   browser at `http://127.0.0.1:<port>/`.
//!
//! Same shim-owns-everything, scalar-only shape as [`crate::run`]: the spawned
//! child + reader threads stream stdout/stderr into a shared buffer that
//! [`WebPlayground::pump`] folds into the line list each frame; build errors
//! (`MTxxxx`) are detected for a toast; the served URL is read back so the IDE
//! can open the default browser. The IDE never blocks on the process.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, MutexGuard,
};
use std::thread;
use std::time::{Duration, Instant};

use crate::diagnostics;

pub(crate) const MAX_WEB_BUILD_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// One line of serve/build output, classified for tinting (error rows red).
#[derive(Debug, Clone)]
pub struct WebLine {
    pub text: String,
    pub is_error: bool,
}

impl WebLine {
    fn plain(text: String) -> Self {
        let is_error = looks_error(&text);
        WebLine { text, is_error }
    }
}

/// Which command backs the running playground (for the header + behavior).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Idle,
    /// `mty serve` for a web-game package.
    Serve,
    /// `mty build --target wasm32-web` + a static file server (fallback).
    Build,
}

/// Shim-owned Web Playground state: the spawned server child + reader thread,
/// the parsed output, the scraped URL, and timing/status.
#[derive(Default)]
pub struct WebPlayground {
    /// `true` while the Web panel is shown.
    active: bool,
    /// Build/serve output lines (stdout+stderr interleaved, ANSI-stripped).
    lines: Vec<WebLine>,
    /// Top visible line (scroll offset).
    first: usize,
    /// `true` while the server child is alive.
    running: bool,
    /// Which command is backing the current session.
    mode: Mode,
    /// The served URL once scraped from the output (empty until then).
    url: String,
    /// `true` once [`pump`] first scrapes a URL — latched for a one-shot
    /// "open the browser" by the IDE.
    url_fresh: bool,
    /// The path/package the session was started for (for the header).
    path: String,
    /// Wall-clock ms since the session started (frozen when stopped).
    duration_ms: u128,
    /// Carry buffer for a partial last line.
    partial: String,
    /// `true` once a build error (`MTxxxx`) was seen — latched for a toast.
    saw_error: bool,
    /// Set on the running→stopped transition; read+cleared by the IDE.
    just_finished: bool,

    // ---- background process plumbing ----
    out: Option<Arc<Mutex<Vec<u8>>>>,
    done: Option<Receiver<()>>,
    child: Option<std::process::Child>,
    started: Option<Instant>,
    /// The temp dir holding the build-fallback harness (cleaned on stop).
    temp_dir: Option<PathBuf>,
}

impl WebPlayground {
    pub fn new() -> Self {
        WebPlayground::default()
    }

    pub fn is_active(&self) -> bool {
        self.active
    }
    pub fn open(&mut self) {
        self.active = true;
    }
    pub fn close(&mut self) {
        self.active = false;
    }
    pub fn toggle(&mut self) -> bool {
        self.active = !self.active;
        self.active
    }
    pub fn is_running(&self) -> bool {
        self.running
    }
    pub fn mode(&self) -> Mode {
        self.mode
    }
    pub fn url(&self) -> &str {
        &self.url
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn duration_ms(&self) -> u128 {
        self.duration_ms
    }
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
    /// Clear rendered build/serve output while preserving URL and process state.
    pub fn clear_output(&mut self) -> usize {
        let n = self.lines.len();
        self.lines.clear();
        self.partial.clear();
        self.first = 0;
        self.saw_error = false;
        n
    }
    pub fn first(&self) -> usize {
        self.first
    }
    pub fn line(&self, i: usize) -> Option<&WebLine> {
        self.lines.get(i)
    }

    /// A concise summary of the latest error-looking output line, suitable for
    /// a toast. The full output remains in the Web panel.
    pub fn latest_error_summary(&self) -> Option<String> {
        let mut generic_exit_summary = None;
        for line in self.lines.iter().rev().filter(|line| line.is_error) {
            if is_generic_build_exit_summary(&line.text) {
                generic_exit_summary.get_or_insert(&line.text);
                continue;
            }
            return Some(compact_error_line(&line.text, 120));
        }
        generic_exit_summary.map(|line| compact_error_line(line, 120))
    }

    /// Read+clear the "a fresh URL was scraped" latch (so the IDE opens the
    /// browser exactly once per server start).
    pub fn take_url_fresh(&mut self) -> bool {
        std::mem::take(&mut self.url_fresh)
    }

    /// Read+clear the running→stopped latch (one-shot, for a toast).
    pub fn take_just_finished(&mut self) -> bool {
        std::mem::take(&mut self.just_finished)
    }

    /// Read+clear the "saw a build error" latch (one-shot, for an error toast).
    pub fn take_saw_error(&mut self) -> bool {
        std::mem::take(&mut self.saw_error)
    }

    pub fn scroll(&mut self, delta: i32) {
        let max = self.lines.len().saturating_sub(1) as i32;
        let mut f = self.first as i32 + delta;
        if f < 0 {
            f = 0;
        }
        if f > max.max(0) {
            f = max.max(0);
        }
        self.first = f as usize;
    }

    pub fn scroll_to_end(&mut self, visible_rows: usize) {
        let n = self.lines.len();
        self.first = n.saturating_sub(visible_rows.max(1));
    }

    /// Resolve the path to the `mty` compiler. Shared shape with `run::mty_path`.
    fn mty_path() -> String {
        crate::mty::path()
    }

    /// The package directory for `file`: the nearest ancestor with a
    /// `mighty.toml`, else the file's parent. (Mirrors `TestPanel::package_dir`.)
    pub fn package_dir(file: &Path) -> PathBuf {
        let start = file.parent().unwrap_or(file);
        let mut cur = Some(start);
        while let Some(dir) = cur {
            if dir.join("mighty.toml").exists() {
                return dir.to_path_buf();
            }
            cur = dir.parent();
        }
        start.to_path_buf()
    }

    /// Decide the mode for `file`: [`Mode::Serve`] when its package has both a
    /// `mighty.toml` and a `web/` dir (the `mty serve` shape), else
    /// [`Mode::Build`].
    pub fn decide_mode(file: &Path) -> Mode {
        let pkg = Self::package_dir(file);
        if pkg.join("mighty.toml").exists() && pkg.join("web").is_dir() {
            Mode::Serve
        } else {
            Mode::Build
        }
    }

    /// Start the playground for `file`. Picks [`Mode::Serve`] or [`Mode::Build`],
    /// spawns the backing process on reader threads, and opens the panel.
    /// Returns `true` if a process spawned. `port` is the bind port.
    pub fn start(&mut self, file: &Path, port: u16) -> bool {
        self.stop();
        self.reset_for(file);
        match Self::decide_mode(file) {
            Mode::Serve => self.start_serve(file, port),
            _ => self.start_build(file, port),
        }
    }

    /// Open the panel with a preflight failure before any build/server process
    /// is spawned. Used by ABI callers that can reject stale active paths.
    pub fn fail_before_start(&mut self, file: &Path, message: impl Into<String>) {
        self.stop();
        self.reset_for(file);
        self.mode = Mode::Idle;
        self.push_line(message.into());
        self.saw_error = true;
        self.finish_now(-1);
    }

    fn reset_for(&mut self, file: &Path) {
        self.lines.clear();
        self.partial.clear();
        self.first = 0;
        self.url.clear();
        self.url_fresh = false;
        self.duration_ms = 0;
        self.saw_error = false;
        self.just_finished = false;
        self.path = file.to_string_lossy().into_owned();
        self.active = true;
    }

    /// `mty serve --port <port> --manifest-dir <pkg>` for a web-game package.
    fn start_serve(&mut self, file: &Path, port: u16) -> bool {
        self.mode = Mode::Serve;
        let pkg = Self::package_dir(file);
        let mty = Self::mty_path();
        let mut cmd = Command::new(&mty);
        cmd.arg("serve")
            .arg("--port")
            .arg(port.to_string())
            .arg("--manifest-dir")
            .arg(&pkg);
        cmd.current_dir(&pkg);
        self.push_line(format!(
            "$ mty serve --port {port} --manifest-dir {}",
            pkg.display()
        ));
        self.spawn(cmd)
    }

    /// Fallback: `mty build --target wasm32-web <file>` synchronously, write a
    /// minimal HTML harness next to the `.wasm` in a temp dir, then serve that
    /// dir with Python's `http.server` on `port`.
    fn start_build(&mut self, file: &Path, port: u16) -> bool {
        self.mode = Mode::Build;
        let mty = Self::mty_path();
        let stem = file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "main".to_string());

        // Build into a fresh temp dir so the harness + wasm sit together.
        let temp = std::env::temp_dir().join(format!("mui-web-{}-{}", std::process::id(), stem));
        if let Err(e) = std::fs::create_dir_all(&temp) {
            self.push_line(format!(
                "failed to create temp web output dir {}: {e}",
                temp.display()
            ));
            self.saw_error = true;
            self.finish_now(-1);
            return false;
        }
        self.temp_dir = Some(temp.clone());

        self.push_line(format!(
            "$ mty build --target wasm32-web {} --out-dir {}",
            file.display(),
            temp.display()
        ));
        let mut build = Command::new(&mty);
        build
            .arg("build")
            .arg("--target")
            .arg("wasm32-web")
            .arg(file)
            .arg("--out-dir")
            .arg(&temp);
        let (status, stdout, stderr) = match run_web_build_command(build) {
            Ok(Some(o)) => o,
            Ok(None) => {
                self.push_line("mty build output too large".to_string());
                self.saw_error = true;
                self.finish_now(-1);
                return false;
            }
            Err(e) => {
                self.push_line(format!("failed to run `{mty} build`: {e}"));
                self.saw_error = true;
                self.finish_now(-1);
                return false;
            }
        };
        // Fold the build's combined output into the panel.
        for stream in [&stdout, &stderr] {
            if !stream.is_empty() {
                let txt = String::from_utf8_lossy(stream);
                self.feed(&txt);
            }
        }
        self.flush_partial();
        let wasm = temp.join(format!("{stem}.wasm"));
        if !status.success() {
            self.push_line(format!("build failed (exit {:?})", status.code()));
            self.saw_error = true;
            self.finish_now(status.code().unwrap_or(-1));
            return false;
        }
        if !wasm_artifact_ready(&wasm) {
            self.push_line(format!(
                "build failed: missing wasm artifact {}",
                wasm.display()
            ));
            self.saw_error = true;
            self.finish_now(-1);
            return false;
        }
        self.push_line(format!("built {} ({} bytes)", wasm.display(), wasm_size(&wasm)));

        // Write the harness page (serves the built wasm at /main.wasm-equivalent).
        if let Err(e) = write_harness(&temp, &stem) {
            self.push_line(format!("failed to write harness: {e}"));
            self.saw_error = true;
            self.finish_now(-1);
            return false;
        }

        // Serve the temp dir statically. Prefer Python; fall back to an error
        // line the user can act on.
        let py = python_exe();
        let mut cmd = Command::new(&py);
        cmd.arg("-m")
            .arg("http.server")
            .arg(port.to_string())
            .arg("--bind")
            .arg("127.0.0.1");
        cmd.current_dir(&temp);
        self.push_line(format!("$ {py} -m http.server {port} --bind 127.0.0.1"));
        // The static server prints its banner to stderr; we synthesize the URL
        // ourselves since `http.server`'s banner has no scheme we can scrape.
        let spawned = self.spawn(cmd);
        if spawned {
            self.set_url(format!("http://127.0.0.1:{port}/"));
        }
        spawned
    }

    /// Spawn `cmd` with piped stdout+stderr on reader threads (the [`crate::run`]
    /// pattern). Sets `running`. Returns `true` if it spawned.
    fn spawn(&mut self, mut cmd: Command) -> bool {
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.push_line(format!("failed to spawn: {e}"));
                self.saw_error = true;
                self.finish_now(-1);
                return false;
            }
        };
        let out = Arc::new(Mutex::new(Vec::<u8>::new()));
        let (done_tx, done_rx) = mpsc::channel();
        let spawn_reader = |mut pipe: Box<dyn Read + Send>, sink: Arc<Mutex<Vec<u8>>>| {
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    match pipe.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            lock_output_buffer(&sink).extend_from_slice(&buf[..n]);
                        }
                    }
                }
            })
        };
        let mut handles = Vec::new();
        if let Some(so) = child.stdout.take() {
            handles.push(spawn_reader(Box::new(so), Arc::clone(&out)));
        }
        if let Some(se) = child.stderr.take() {
            handles.push(spawn_reader(Box::new(se), Arc::clone(&out)));
        }
        std::thread::spawn(move || {
            for h in handles {
                let _ = h.join();
            }
            let _ = done_tx.send(());
        });
        self.out = Some(out);
        self.done = Some(done_rx);
        self.child = Some(child);
        self.started = Some(Instant::now());
        self.running = true;
        true
    }

    /// Stop the server (best-effort kill + reap) + clean the temp harness dir.
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.out = None;
        self.done = None;
        if self.running {
            self.running = false;
            self.just_finished = true;
            if let Some(s) = self.started.take() {
                self.duration_ms = s.elapsed().as_millis();
            }
        }
        self.started = None;
        if let Some(dir) = self.temp_dir.take() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// Mark the session finished synchronously (build failed before a child, or
    /// a spawn error). Records the duration + status.
    fn finish_now(&mut self, _code: i32) {
        self.running = false;
        if let Some(s) = self.started.take() {
            self.duration_ms = s.elapsed().as_millis();
        }
    }

    /// Drain pending output into the line list, scrape the URL, detect
    /// completion. Returns `true` if anything changed (so the IDE redraws).
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        if let Some(out) = &self.out {
            let chunk = {
                let mut g = lock_output_buffer(out);
                if g.is_empty() {
                    Vec::new()
                } else {
                    std::mem::take(&mut *g)
                }
            };
            if !chunk.is_empty() {
                let text = String::from_utf8_lossy(&chunk).into_owned();
                self.feed(&text);
                changed = true;
            }
        }
        if self.running {
            if let Some(done) = &self.done {
                match done.try_recv() {
                    Ok(()) | Err(TryRecvError::Disconnected) => {
                        if let Some(out) = self.out.take() {
                            let g = lock_output_buffer(&out);
                            if !g.is_empty() {
                                let text = String::from_utf8_lossy(&g).into_owned();
                                self.feed(&text);
                            }
                        }
                        self.flush_partial();
                        self.running = false;
                        if let Some(mut child) = self.child.take() {
                            let _ = child.wait();
                        }
                        if let Some(s) = self.started.take() {
                            self.duration_ms = s.elapsed().as_millis();
                        }
                        self.done = None;
                        self.just_finished = true;
                        changed = true;
                    }
                    Err(TryRecvError::Empty) => {}
                }
            }
        }
        changed
    }

    /// Append a (possibly partial) chunk: split on newlines, carry the tail,
    /// scrape a served URL out of each completed line.
    fn feed(&mut self, chunk: &str) {
        let clean = diagnostics::strip_ansi_public(chunk);
        let mut buf = std::mem::take(&mut self.partial);
        buf.push_str(&clean);
        let buf = buf.replace("\r\n", "\n").replace('\r', "\n");
        let mut parts: Vec<&str> = buf.split('\n').collect();
        let tail = parts.pop().unwrap_or("").to_string();
        for line in parts {
            self.push_line(line.to_string());
        }
        self.partial = tail;
    }

    fn flush_partial(&mut self) {
        if !self.partial.is_empty() {
            let p = std::mem::take(&mut self.partial);
            self.push_line(p);
        }
    }

    fn push_line(&mut self, text: String) {
        if let Some(u) = extract_url(&text) {
            self.set_url(u);
        }
        let l = WebLine::plain(text);
        if l.is_error {
            self.saw_error = true;
        }
        self.lines.push(l);
    }

    fn set_url(&mut self, url: String) {
        if self.url != url {
            self.url = url;
            self.url_fresh = true;
        }
    }

    /// Seed fake serve output (used by the screenshot hook so the panel renders
    /// without spawning a real server).
    pub fn seed_demo(&mut self, path: &str) {
        self.path = path.to_string();
        self.active = true;
        self.running = true;
        self.mode = Mode::Serve;
        self.duration_ms = 0;
        self.lines.clear();
        self.first = 0;
        self.url.clear();
        let demo = [
            "$ mty serve --port 8000 --manifest-dir examples/webspin",
            "compiling webspin (--target wasm32-web)",
            "  finished: target/main.wasm (Component Model, 2172 bytes)",
            "serving web/ + /main.wasm",
            "mty serve: listening on http://127.0.0.1:8000",
            "[watch] src/ -> rebuild on save (ws /_reload)",
        ];
        for d in demo {
            self.push_line(d.to_string());
        }
    }
}

fn lock_output_buffer(out: &Arc<Mutex<Vec<u8>>>) -> MutexGuard<'_, Vec<u8>> {
    out.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn run_web_build_command(
    mut cmd: Command,
) -> Result<Option<(ExitStatus, Vec<u8>, Vec<u8>)>, String> {
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "stdout pipe unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "stderr pipe unavailable".to_string())?;
    let exceeded = Arc::new(AtomicBool::new(false));
    let stdout_exceeded = Arc::clone(&exceeded);
    let stderr_exceeded = Arc::clone(&exceeded);
    let stdout_reader = thread::spawn(move || {
        read_web_build_stream_capped(stdout, MAX_WEB_BUILD_OUTPUT_BYTES, stdout_exceeded)
    });
    let stderr_reader = thread::spawn(move || {
        read_web_build_stream_capped(stderr, MAX_WEB_BUILD_OUTPUT_BYTES, stderr_exceeded)
    });

    let status = loop {
        if exceeded.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Ok(None);
        }
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status,
            None => thread::sleep(Duration::from_millis(10)),
        }
    };

    let stdout = stdout_reader
        .join()
        .map_err(|_| "stdout reader panicked".to_string())?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "stderr reader panicked".to_string())?;
    if exceeded.load(Ordering::Relaxed) {
        Ok(None)
    } else {
        Ok(Some((status, stdout, stderr)))
    }
}

fn read_web_build_stream_capped<R: Read>(
    mut reader: R,
    cap: usize,
    exceeded: Arc<AtomicBool>,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if !append_web_build_output_with_cap(&mut out, &chunk[..n], cap) {
                    exceeded.store(true, Ordering::Relaxed);
                    break;
                }
            }
            Err(_) => break,
        }
    }
    out
}

fn append_web_build_output_with_cap(out: &mut Vec<u8>, chunk: &[u8], cap: usize) -> bool {
    if out.len().saturating_add(chunk.len()) > cap {
        out.clear();
        return false;
    }
    out.extend_from_slice(chunk);
    true
}

/// Open `url` in the default browser. Windows: `cmd /c start "" <url>`.
/// Returns `true` if the launcher process spawned.
pub fn open_in_browser(url: &str) -> bool {
    open_in_browser_result(url).is_ok()
}

/// Open `url` in the default browser, preserving the launcher/spawn failure.
pub fn open_in_browser_result(url: &str) -> Result<(), String> {
    if url.is_empty() {
        return Err("URL not ready".to_string());
    }
    if let Ok(reason) = std::env::var("MUI_WEB_BROWSER_OPEN_FAIL") {
        let reason = reason.trim();
        return Err(if reason.is_empty() {
            "browser launcher unavailable".to_string()
        } else {
            reason.to_string()
        });
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(url).spawn().map(|_| ()).map_err(|e| e.to_string())
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(url).spawn().map(|_| ()).map_err(|e| e.to_string())
    }
}

/// Scrape an `http://…`/`https://…` URL out of a serve-output line. Returns the
/// first URL token (trimmed of trailing punctuation). The canonical line is
/// `mty serve: listening on http://127.0.0.1:8000`.
pub fn extract_url(line: &str) -> Option<String> {
    let pos = line.find("http://").or_else(|| line.find("https://"))?;
    let rest = &line[pos..];
    // Take up to the first whitespace; strip trailing sentence punctuation.
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let url = rest[..end].trim_end_matches(['.', ',', ')', ']', '"', '\'']);
    if is_local_dev_server_url(url) {
        Some(url.to_string())
    } else {
        None
    }
}

/// Pull a `:<port>` out of a scraped URL (for tests / status). `None` if absent.
pub fn port_of(url: &str) -> Option<u16> {
    let (_, port) = parse_local_dev_url_authority(url)?;
    Some(port)
}

fn is_local_dev_server_url(url: &str) -> bool {
    parse_local_dev_url_authority(url).is_some()
}

fn parse_local_dev_url_authority(url: &str) -> Option<(&str, u16)> {
    let after_scheme = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let authority = after_scheme.split('/').next()?.split('?').next()?.split('#').next()?;
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let close = rest.find(']')?;
        let host = &rest[..close];
        let port = rest[close + 1..].strip_prefix(':')?;
        (host, port)
    } else {
        let (host, port) = authority.rsplit_once(':')?;
        if host.contains(':') {
            return None;
        }
        (host, port)
    };
    if !is_loopback_host(host) {
        return None;
    }
    let port = parse_positive_port(port)?;
    Some((host, port))
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "::1" || host.starts_with("127.")
}

fn parse_positive_port(raw: &str) -> Option<u16> {
    crate::parse_unsigned_decimal_u16_token(raw).filter(|port| *port > 0)
}

/// `true` if a line looks like a build error (an `[MTxxxx]` header, `Error:`,
/// or cargo-style `error:`).
fn looks_error(line: &str) -> bool {
    line.contains("] Error:")
        || line.contains("error:")
        || (line.contains("[MT") && line.contains("Error"))
        || line.starts_with("build failed")
        || line.starts_with("failed to")
}

fn is_generic_build_exit_summary(line: &str) -> bool {
    line.starts_with("build failed (exit ")
}

fn compact_error_line(line: &str, max_chars: usize) -> String {
    let trimmed = line.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max_chars.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

fn wasm_size(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

fn wasm_artifact_ready(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|meta| meta.is_file())
}

/// Resolve a Python interpreter for the static-server fallback.
fn python_exe() -> String {
    if let Ok(p) = std::env::var("MIGHTY_PYTHON") {
        if !p.trim().is_empty() {
            return p;
        }
    }
    // On Windows the launcher `py` is the most reliable; elsewhere `python3`.
    if cfg!(target_os = "windows") {
        "py".to_string()
    } else {
        "python3".to_string()
    }
}

/// Write a minimal `index.html` harness next to the built `<stem>.wasm` that
/// instantiates the component's inner core module and surfaces its `log` output.
/// The build-fallback path serves this dir statically.
fn write_harness(dir: &Path, stem: &str) -> std::io::Result<()> {
    let html = format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<title>{stem} — Mighty wasm32-web</title>
<style>
 :root {{ color-scheme: dark; }}
 body {{ font-family: ui-sans-serif, system-ui, sans-serif; background:#0b0d12; color:#e7ecf3;
        margin:0; min-height:100vh; display:flex; flex-direction:column; align-items:center;
        padding:2rem 1rem; gap:1rem; }}
 h1 {{ font-size:1.3rem; font-weight:600; margin:0; }} h1 .v {{ color:#b66bff; }}
 #log {{ background:#08090d; color:#9bbcff; padding:.6rem .9rem; border-radius:6px; width:min(100%,40rem);
         height:18rem; overflow:auto; font-family:ui-monospace,monospace; font-size:.8rem; white-space:pre-wrap; }}
 .hint {{ color:#7f8a9c; font-size:.85rem; }}
</style></head><body>
<h1>{stem} <span class="v">Mighty · wasm32-web</span></h1>
<p class="hint">Built by the Mighty IDE via <b>Run in Browser</b> (build fallback). Calling exported entry points + streaming the guest's <code>log</code>.</p>
<pre id="log"></pre>
<script type="module">
const logEl = document.getElementById('log');
const out = (s) => {{ logEl.textContent += s + '\n'; logEl.scrollTop = logEl.scrollHeight; }};
function coreModule(bytes) {{
  for (let i=0;i<bytes.length-8;i++) if (bytes[i]==0&&bytes[i+1]==0x61&&bytes[i+2]==0x73&&bytes[i+3]==0x6d&&bytes[i+4]==1&&bytes[i+5]==0&&bytes[i+6]==0&&bytes[i+7]==0) return bytes.subarray(i);
  throw new Error('no core wasm preamble in component');
}}
const memBox = {{ inst:null }};
const logImp = (ptr,len) => {{ const m = memBox.inst&&memBox.inst.exports.memory; if(!m) return; out(new TextDecoder().decode(new Uint8Array(m.buffer,ptr,len))); }};
(async () => {{
  try {{
    const resp = await fetch('./{stem}.wasm'); if(!resp.ok) throw new Error('fetch '+resp.status);
    const bytes = coreModule(new Uint8Array(await resp.arrayBuffer()));
    const {{ instance }} = await WebAssembly.instantiate(bytes, {{ env:{{log:logImp}}, mty:{{log:logImp}} }});
    memBox.inst = instance; const ex = instance.exports;
    out('[host] component instantiated; exports: ' + Object.keys(ex).join(', '));
    if (ex.start) ex.start();
    let n=0; const id=setInterval(()=>{{ if(ex.tick){{ ex.tick(); if(++n>=120) clearInterval(id); }} else {{ clearInterval(id); }} }}, 16);
  }} catch(e) {{ out('[host] error: ' + e); }}
}})();
</script></body></html>
"#
    );
    let mut f = std::fs::File::create(dir.join("index.html"))?;
    f.write_all(html.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_url_from_serve_banner() {
        let line = "mty serve: listening on http://127.0.0.1:8000";
        assert_eq!(extract_url(line).as_deref(), Some("http://127.0.0.1:8000"));
    }

    #[test]
    fn extract_url_strips_trailing_punctuation() {
        assert_eq!(
            extract_url("open (http://localhost:9000/).").as_deref(),
            Some("http://localhost:9000/")
        );
    }

    #[test]
    fn extract_url_handles_https() {
        assert_eq!(
            extract_url("serving https://127.0.0.1:8443 now").as_deref(),
            Some("https://127.0.0.1:8443")
        );
    }

    #[test]
    fn extract_url_accepts_localhost_and_ipv6_loopback() {
        assert_eq!(
            extract_url("open http://localhost:9000/index.html").as_deref(),
            Some("http://localhost:9000/index.html")
        );
        assert_eq!(
            extract_url("open http://[::1]:8123/").as_deref(),
            Some("http://[::1]:8123/")
        );
    }

    #[test]
    fn extract_url_none_when_absent() {
        assert!(extract_url("compiling webspin (--target wasm32-web)").is_none());
        assert!(extract_url("plain text no url").is_none());
    }

    #[test]
    fn extract_url_rejects_non_local_or_malformed_server_urls() {
        assert!(extract_url("docs: https://example.com:443/help").is_none());
        assert!(extract_url("open http://127.0.0.1/").is_none());
        assert!(extract_url("open http://127.0.0.1:0/").is_none());
        assert!(extract_url("open http://127.0.0.1:+8000/").is_none());
        assert!(extract_url("open http://127.0.0.1:-8000/").is_none());
        assert!(extract_url("open http://127.0.0.1:8e3/").is_none());
        assert!(extract_url("open http://127.0.0.1:70000/").is_none());
        assert!(extract_url("open http://[::1]/").is_none());
        assert!(extract_url("open http://::1:8123/").is_none());
    }

    #[test]
    fn port_of_parses() {
        assert_eq!(port_of("http://127.0.0.1:8000"), Some(8000));
        assert_eq!(port_of("http://localhost:9000/"), Some(9000));
        assert_eq!(port_of("http://[::1]:7000/"), Some(7000));
        assert_eq!(port_of("http://example.com/"), None);
        assert_eq!(port_of("http://example.com:9000/"), None);
        assert_eq!(port_of("http://127.0.0.1:0/"), None);
    }

    #[test]
    fn pump_scrapes_url_and_latches_fresh() {
        let mut w = WebPlayground::new();
        w.feed("mty serve: listening on http://127.0.0.1:8123\n");
        assert_eq!(w.url(), "http://127.0.0.1:8123");
        assert!(w.take_url_fresh());
        // latch cleared after read
        assert!(!w.take_url_fresh());
    }

    #[test]
    fn pump_recovers_from_poisoned_output_buffer() {
        let out = Arc::new(Mutex::new(
            b"mty serve: listening on http://127.0.0.1:8124\n".to_vec(),
        ));
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let poisoned = {
            let out = Arc::clone(&out);
            std::panic::catch_unwind(move || {
                let _guard = lock_output_buffer(&out);
                panic!("poison web output buffer");
            })
        };
        std::panic::set_hook(hook);
        assert!(poisoned.is_err());

        let mut w = WebPlayground::new();
        w.out = Some(out);

        assert!(w.pump());
        assert_eq!(w.line_count(), 1);
        assert_eq!(w.url(), "http://127.0.0.1:8124");
        assert!(w.take_url_fresh());
        assert!(lock_output_buffer(w.out.as_ref().unwrap()).is_empty());
    }

    #[test]
    fn feed_splits_and_carries_partial() {
        let mut w = WebPlayground::new();
        w.feed("compiling...\nlisten");
        assert_eq!(w.line_count(), 1);
        w.feed("ing on http://127.0.0.1:8000\n");
        assert_eq!(w.line_count(), 2);
        assert_eq!(w.url(), "http://127.0.0.1:8000");
    }

    #[test]
    fn error_line_latches_saw_error() {
        let mut w = WebPlayground::new();
        w.feed("[MT2001] Error: expected `I32`, found `Str`\n");
        assert!(w.line(0).unwrap().is_error);
        assert!(w.take_saw_error());
    }

    #[test]
    fn plain_line_is_not_error() {
        let mut w = WebPlayground::new();
        w.feed("serving web/ + /main.wasm\n");
        assert!(!w.line(0).unwrap().is_error);
    }

    #[test]
    fn latest_error_summary_prefers_compiler_error_over_exit_summary() {
        let mut w = WebPlayground::new();
        w.feed("error: expected `I32`, found `Str`\n");
        w.feed("build failed (exit Some(1))\n");

        assert_eq!(
            w.latest_error_summary().as_deref(),
            Some("error: expected `I32`, found `Str`")
        );
    }

    #[test]
    fn latest_error_summary_keeps_exit_summary_when_it_is_only_error() {
        let mut w = WebPlayground::new();
        w.feed("build failed (exit Some(1))\n");

        assert_eq!(
            w.latest_error_summary().as_deref(),
            Some("build failed (exit Some(1))")
        );
    }

    #[test]
    fn latest_error_summary_keeps_missing_artifact_summary() {
        let mut w = WebPlayground::new();
        w.feed("build failed: missing wasm artifact C:\\tmp\\main.wasm\n");

        assert_eq!(
            w.latest_error_summary().as_deref(),
            Some("build failed: missing wasm artifact C:\\tmp\\main.wasm")
        );
    }

    #[test]
    fn web_build_output_cap_accepts_exact_limit() {
        let mut out = Vec::from(b"web");

        assert!(append_web_build_output_with_cap(&mut out, b"!", 4));
        assert_eq!(out, b"web!");
    }

    #[test]
    fn web_build_output_cap_discards_oversized_stream() {
        let mut out = Vec::from(b"web:");

        assert!(!append_web_build_output_with_cap(&mut out, b"overflow", 8));
        assert!(out.is_empty());
    }

    #[test]
    fn web_build_stream_reader_marks_oversized_output() {
        let exceeded = Arc::new(AtomicBool::new(false));
        let out = read_web_build_stream_capped(
            std::io::Cursor::new(b"abcdef".to_vec()),
            5,
            Arc::clone(&exceeded),
        );

        assert!(out.is_empty());
        assert!(exceeded.load(Ordering::Relaxed));
    }

    #[test]
    fn wasm_artifact_ready_requires_a_file() {
        let tmp = std::env::temp_dir().join(format!("mui-web-artifact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("main.wasm");
        let dir = tmp.join("folder.wasm");
        std::fs::write(&file, b"\0asm").unwrap();
        std::fs::create_dir_all(&dir).unwrap();

        assert!(wasm_artifact_ready(&file));
        assert!(!wasm_artifact_ready(&dir));
        assert!(!wasm_artifact_ready(&tmp.join("missing.wasm")));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn decide_mode_serve_when_web_dir_present() {
        let tmp = std::env::temp_dir().join(format!("mui-web-mode-{}", std::process::id()));
        let _ = std::fs::create_dir_all(tmp.join("src"));
        let _ = std::fs::create_dir_all(tmp.join("web"));
        std::fs::write(tmp.join("mighty.toml"), b"[package]\nname=\"x\"\n").unwrap();
        std::fs::write(tmp.join("src/main.mty"), b"package x\nfn main() {}\n").unwrap();
        assert_eq!(WebPlayground::decide_mode(&tmp.join("src/main.mty")), Mode::Serve);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn decide_mode_build_when_no_web_dir() {
        let tmp = std::env::temp_dir().join(format!("mui-web-mode2-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        std::fs::write(tmp.join("standalone.mty"), b"package s\nfn main() {}\n").unwrap();
        assert_eq!(WebPlayground::decide_mode(&tmp.join("standalone.mty")), Mode::Build);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn build_fallback_reports_temp_output_dir_blockers_before_spawn() {
        let stem = format!("blocked_{}", std::process::id());
        let blocker = std::env::temp_dir().join(format!("mui-web-{}-{stem}", std::process::id()));
        let _ = std::fs::remove_dir_all(&blocker);
        let _ = std::fs::remove_file(&blocker);
        std::fs::write(&blocker, b"not a directory").unwrap();

        let mut w = WebPlayground::new();
        let started = w.start_build(&PathBuf::from(format!("{stem}.mty")), 8123);

        assert!(!started);
        assert!(!w.is_running());
        assert!(w.take_saw_error());
        let line = &w.line(0).unwrap().text;
        assert!(
            line.starts_with("failed to create temp web output dir "),
            "{line}"
        );
        assert!(line.contains(&stem), "{line}");

        let _ = std::fs::remove_file(&blocker);
    }

    #[test]
    fn seed_demo_has_url() {
        let mut w = WebPlayground::new();
        w.seed_demo("examples/webspin/src/main.mty");
        assert!(w.line_count() > 0);
        assert_eq!(w.url(), "http://127.0.0.1:8000");
        assert_eq!(w.mode(), Mode::Serve);
        assert!(w.is_running());
    }

    #[test]
    fn clear_output_preserves_url_and_session_state() {
        let mut w = WebPlayground::new();
        w.seed_demo("examples/webspin/src/main.mty");
        w.scroll(10);
        let url_fresh = w.take_url_fresh();

        let cleared = w.clear_output();

        assert!(url_fresh);
        assert_eq!(cleared, 6);
        assert_eq!(w.line_count(), 0);
        assert_eq!(w.first(), 0);
        assert_eq!(w.url(), "http://127.0.0.1:8000");
        assert_eq!(w.mode(), Mode::Serve);
        assert_eq!(w.path(), "examples/webspin/src/main.mty");
        assert!(w.is_active());
        assert!(w.is_running());
        assert!(!w.take_saw_error());
    }

    #[test]
    fn scroll_clamps() {
        let mut w = WebPlayground::new();
        w.feed("a\nb\nc\n");
        w.scroll(10);
        assert_eq!(w.first(), 2);
        w.scroll(-10);
        assert_eq!(w.first(), 0);
    }

    #[test]
    fn open_in_browser_empty_is_noop() {
        assert!(!open_in_browser(""));
    }

    /// Guarded integration test: build the pure sample to wasm32-web and assert a
    /// `.wasm` is produced. Skips if `mty` is unavailable or the build fails (so
    /// CI without the dev compiler stays green).
    #[test]
    fn build_pure_sample_to_wasm_or_skip() {
        let mty = WebPlayground::mty_path();
        if mty == "mty" && Command::new("mty").arg("--version").output().is_err() {
            eprintln!("SKIP: mty not available");
            return;
        }
        // The pure sample lives at <repo>/examples/webspin/src/main.mty. Resolve
        // from CARGO_MANIFEST_DIR (the crate dir) up to the repo root.
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let sample = manifest
            .parent()
            .and_then(|p| p.parent())
            .map(|root| root.join("examples/webspin/src/main.mty"));
        let Some(sample) = sample.filter(|p| p.exists()) else {
            eprintln!("SKIP: webspin sample not found");
            return;
        };
        let out = std::env::temp_dir().join(format!("mui-web-it-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&out);
        let res = Command::new(&mty)
            .arg("build")
            .arg("--target")
            .arg("wasm32-web")
            .arg(&sample)
            .arg("--out-dir")
            .arg(&out)
            .output();
        match res {
            Ok(o) if o.status.success() => {
                let wasm = out.join("main.wasm");
                assert!(wasm.exists(), "expected {} to exist", wasm.display());
                assert!(wasm_size(&wasm) > 8, "wasm should be non-trivial");
                eprintln!("OK: built {} ({} bytes)", wasm.display(), wasm_size(&wasm));
            }
            Ok(o) => eprintln!(
                "SKIP: mty build wasm32-web failed (exit {:?})",
                o.status.code()
            ),
            Err(e) => eprintln!("SKIP: could not spawn mty build: {e}"),
        }
        let _ = std::fs::remove_dir_all(&out);
    }
}
