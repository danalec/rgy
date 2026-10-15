//! Long-tail adapters: external tools only.
//!
//! The external-tool adapters delegate to binaries already on the machine
//! (soffice, tesseract). Each detects its binary on PATH in `available()`;
//! when the tool is missing the adapter is skipped entirely, so a minimal
//! machine still gets the full in-process adapter set. Nothing here is
//! spawned unless a matching file is actually found.

use rgx_core::{bail, Context, Result};
use rgx_core::{Adapter, Outcome};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Wall-clock limit on a single tool invocation. Soffice can spend tens
/// of seconds on a multi-hundred-page document; tesseract normally
/// finishes in one or two, but a hostile image can spin forever. Thirty
/// seconds is enough headroom for both while still bounding the user.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

pub fn adapters() -> Vec<Box<dyn Adapter>> {
    vec![Box::new(Soffice), Box::new(tesseract())]
}

/// Adapter that pipes bytes to a tool's stdin and captures stdout.
pub struct ProcAdapter {
    name: &'static str,
    /// candidate binary names, first hit on PATH wins
    binaries: &'static [&'static str],
    /// argv built from the original path (informational — data goes via stdin)
    args: fn(&str) -> Vec<String>,
    exts: &'static [&'static str],
    /// optional post-processing of captured stdout
    post: fn(Vec<u8>) -> Vec<u8>,
    /// wall-clock budget per invocation; a run that exceeds it is killed
    timeout: Duration,
}

fn which(binary: &str) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    // The PATH does not change during a run: probe each candidate binary
    // once. Without this cache, `available()` costs ~20 filesystem stats
    // per adapter per file — that dominated whole-corpus searches.
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(&hit) = cache.lock().unwrap().get(binary) {
        return hit;
    }
    let found = which_uncached(binary);
    cache.lock().unwrap().insert(binary.to_string(), found);
    found
}

fn which_uncached(binary: &str) -> bool {
    // explicit path separators → check directly
    if binary.contains('/') || binary.contains('\\') {
        return std::path::Path::new(binary).exists();
    }
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| {
        // `mut` is exercised only by the Windows extension probing below;
        // on any other target it is a warning on every build.
        #[cfg_attr(not(windows), allow(unused_mut))]
        let mut p = dir.join(binary);
        if p.is_file() {
            return true;
        }
        // Windows: also try .exe / .bat / .cmd
        #[cfg(windows)]
        {
            for ext in ["exe", "bat", "cmd", "ps1"] {
                p.set_extension(ext);
                if p.is_file() {
                    return true;
                }
            }
        }
        false
    })
}

fn temp_path() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "rgx-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    p
}

/// Drains the child's stdout and stderr on background threads, waits for
/// it to exit on a third, and kills it if `timeout` elapses first. The
/// drainers are necessary: leaving the pipes unread would let a child
/// block on a full pipe buffer and never reach `wait()`.
fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> std::io::Result<std::process::Output> {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let child = Arc::new(Mutex::new(Some(child)));

    fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> Option<thread::JoinHandle<Vec<u8>>> {
        pipe.map(|mut s| {
            thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = s.read_to_end(&mut buf);
                buf
            })
        })
    }
    let stdout_handle = drain(stdout);
    let stderr_handle = drain(stderr);

    let (tx, rx) = mpsc::channel::<std::io::Result<std::process::ExitStatus>>();
    let child_w = Arc::clone(&child);
    let waiter = thread::spawn(move || {
        let result = {
            let mut guard = child_w.lock().unwrap();
            match guard.as_mut() {
                Some(c) => c.wait(),
                None => Err(std::io::Error::other("child vanished")),
            }
        };
        let _ = tx.send(result);
    });

    let start = Instant::now();
    let poll = Duration::from_millis(100);
    let status = loop {
        match rx.recv_timeout(poll) {
            Ok(Ok(s)) => break Ok(s),
            Ok(Err(e)) => break Err(e),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if start.elapsed() >= timeout {
                    if let Some(c) = child.lock().unwrap().as_mut() {
                        let _ = c.kill();
                    }
                    break Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("process exceeded {}s timeout", timeout.as_secs()),
                    ));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break Err(std::io::Error::other("waiter disconnected"));
            }
        }
    };

    let _ = waiter.join();
    let stdout = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();

    Ok(std::process::Output {
        status: status?,
        stdout,
        stderr,
    })
}

impl Adapter for ProcAdapter {
    fn name(&self) -> &'static str {
        self.name
    }

    fn extensions(&self) -> &'static [&'static str] {
        self.exts
    }

    fn available(&self) -> bool {
        self.binaries.iter().any(|b| which(b))
    }

    fn extract(&self, data: &[u8], path: &str) -> Result<Outcome> {
        let binary = self
            .binaries
            .iter()
            .copied()
            .find(|b| which(b))
            .context("binary vanished between detection and spawn")?;
        let argv: Vec<String> = (self.args)(path);
        let mut cmd = Command::new(binary);
        cmd.args(&argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return Err(e).context("spawn failed"),
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(data);
        }
        let out = match wait_with_timeout(child, self.timeout) {
            Ok(o) => o,
            Err(e) => return Err(e).context("wait failed"),
        };
        if !out.status.success() || out.stdout.is_empty() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stderr = stderr.trim();
            if stderr.is_empty() {
                bail!("{} exited with {}", binary, out.status);
            }
            bail!(
                "{} exited with {}: {}",
                binary,
                out.status,
                stderr.lines().next().unwrap_or("")
            );
        }
        let stdout = (self.post)(out.stdout);
        if stdout.is_empty() {
            bail!("{} produced no text", binary);
        }
        Ok(Outcome::Text(stdout))
    }
}

const IDENTITY: fn(Vec<u8>) -> Vec<u8> = |b| b;

// ffmpeg/ffprobe used to cover avi/ts/mpg/mpeg/wmv/flv here; the media
// crate's in-process adapters own all of them now (avi/asf/flv/mpeg
// readers ported from upstream), and .srt is plain text for raw search —
// so the external adapter is gone.

// legacy .doc used to live here (antiword); the office crate's in-process
// FIB + piece-table reader owns .doc now, so the external adapter is gone.

/// LibreOffice headless (fallback for formats nothing else reads well).
/// Custom `Adapter` impl: soffice converts *to a file* in --outdir rather
/// than stdout, so we point it at a scratch dir and read the `.txt` back.
pub struct Soffice;
impl Adapter for Soffice {
    fn name(&self) -> &'static str {
        "soffice"
    }
    fn extensions(&self) -> &'static [&'static str] {
        // the whole legacy-office surface is in-process now (office crate:
        // ppt/pptx/xls/xlsx/ods/odp + binary Visio .vsd); what remains for
        // soffice is the one format nothing else reads: MS Publisher (.pub)
        &["pub"]
    }
    fn available(&self) -> bool {
        which("soffice") || which("libreoffice")
    }
    fn extract(&self, data: &[u8], path: &str) -> Result<Outcome> {
        let binary = if which("soffice") {
            "soffice"
        } else {
            "libreoffice"
        };
        let ext = rgx_core::extension_of(path);
        let src = temp_path();
        let src = src.with_extension(&*ext);
        let outdir = temp_path();
        std::fs::create_dir_all(&outdir)?;
        std::fs::write(&src, data)?;
        let out_txt = outdir.join(src.with_extension("txt").file_name().unwrap());
        let mut cmd = Command::new(binary);
        cmd.args([
            "--headless".into(),
            "--convert-to".into(),
            "txt:Text".into(),
            "--outdir".into(),
            outdir.to_string_lossy().into_owned(),
            src.to_string_lossy().into_owned(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        let child = cmd.spawn().context("spawn soffice")?;
        let output = wait_with_timeout(child, DEFAULT_TIMEOUT).context("wait soffice")?;
        let status = output.status;
        let _ = std::fs::remove_file(&src);
        let text = std::fs::read(&out_txt).unwrap_or_default();
        let _ = std::fs::remove_file(&out_txt);
        let _ = std::fs::remove_dir(&outdir);
        if !status.success() || text.is_empty() {
            bail!("soffice exited with {status}");
        }
        Ok(Outcome::Text(text))
    }
}

/// OCR for scanned documents. Only claims explicit `*.ocr.*`-style double
/// extensions so ordinary photos never trigger an OCR run by default.
pub fn tesseract() -> ProcAdapter {
    ProcAdapter {
        name: "tesseract",
        binaries: &["tesseract"],
        args: |_| vec!["stdin".into(), "stdout".into(), "--psm".into(), "3".into()],
        exts: &["ocrjpg", "ocrjpeg", "ocrpng", "ocrtiff", "ocrpdf"],
        post: IDENTITY,
        timeout: DEFAULT_TIMEOUT,
    }
}

// openssl x509 used to live here (pem/crt/cer/der/cert); the inspect
// crate's in-process DER/PEM walker (reduced port of upstream x509) owns
// certificates now, so the external adapter is gone.

// catdoc's xls2csv used to cover legacy .xls; the office crate's
// in-process BIFF reader owns it now, so the external adapter is gone.

// exiftool used to cover gif/bmp images here; the media crate's
// in-process imgmeta adapters (ported from upstream io::binary) own them
// now, so the external adapter is gone.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_finds_something() {
        // every machine has at least one of these; smoke test of PATH scanning
        let found = ["sh", "cmd.exe", "ffmpeg"].iter().any(|b| which(b));
        assert!(found, "PATH scan should find at least one shell");
    }

    #[test]
    fn wait_with_timeout_kills_a_hung_child() {
        // A shell that sleeps past the deadline: wait_with_timeout must
        // return a TimedOut error and never block past the deadline
        // (a loose 30s cap is plenty of headroom for OS process cleanup;
        // the win is going from "infinite" to "bounded").
        let shell = if cfg!(windows) { "cmd.exe" } else { "sh" };
        let arg = if cfg!(windows) { "/c" } else { "-c" };
        let mut cmd = Command::new(shell);
        cmd.args([
            arg,
            if cfg!(windows) {
                "ping -n 5 127.0.0.1 > nul"
            } else {
                "sleep 5"
            },
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let child = cmd.spawn().expect("shell should spawn");
        let start = std::time::Instant::now();
        let out = wait_with_timeout(child, Duration::from_millis(500)).unwrap_err();
        let elapsed = start.elapsed();
        assert_eq!(out.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            elapsed < Duration::from_secs(30),
            "wait_with_timeout must not block past 30s, took {elapsed:?}"
        );
    }

    #[test]
    fn wait_with_timeout_returns_a_clean_result_for_a_normal_child() {
        let shell = if cfg!(windows) { "cmd.exe" } else { "sh" };
        let arg = if cfg!(windows) { "/c" } else { "-c" };
        let mut cmd = Command::new(shell);
        cmd.args([arg, "echo hi"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = cmd.spawn().expect("shell should spawn");
        let out = wait_with_timeout(child, Duration::from_secs(5)).unwrap();
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains("hi"));
    }
}
