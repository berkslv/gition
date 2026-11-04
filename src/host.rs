//! What the app needs from its environment besides files: syncing and
//! opening things outside gition. The terminal build uses the git CLI, the web
//! build the GitHub API.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use anyhow::Result;

use crate::sync::State;

/// Results of background work, applied in `App::tick`.
pub enum Msg {
    /// Fetch finished; the number of commits fast-forwarded.
    Fetched(State, usize),
    Synced(Result<String, String>, State),
    /// A page's title, or none if it couldn't be read, for a URL pasted into the editor.
    Title(String, Option<String>),
    /// A shell command finished: the command, its output (`Err` if it failed) and the repo state after it.
    Ran(String, Result<String, String>, State),
    /// Images compressed for a page, as `(file name, bytes)`, ready to go under the assets dir.
    Images(PathBuf, Vec<(String, Vec<u8>)>),
}

pub enum Open<'a> {
    Url(&'a str),
    /// A repo-relative file.
    File(&'a Path),
}

pub trait Host {
    /// Fetches in the background, fast-forwarding when nothing local would be lost.
    /// Sends [`Msg::Fetched`].
    fn fetch(&self, tx: Sender<Msg>);
    /// Commits every change, pulls and pushes in the background. Sends [`Msg::Synced`].
    fn sync(&self, commit_prefix: &str, tx: Sender<Msg>);
    /// Number of uncommitted changed paths.
    fn changes(&self) -> usize;
    fn open(&self, target: Open) -> Result<()>;
    /// Starts a file picker for an image, if the host has one. When it returns
    /// false, the app asks for a path instead. Picked files come back through
    /// `App::insert_image_data`.
    fn pick_image(&self) -> bool {
        false
    }
    /// Puts text on the system clipboard. False if the host has no way to.
    fn copy(&self, _text: &str) -> bool {
        false
    }
    /// Reads the title of the page at `url` in the background. Sends [`Msg::Title`].
    fn fetch_title(&self, _url: &str, _tx: Sender<Msg>) {}
    /// The image on the system clipboard as PNG, if the host can read one.
    fn paste_image(&self) -> Option<Vec<u8>> {
        None
    }
    /// Runs a shell command in the repo in the background. Sends [`Msg::Ran`].
    /// False if the host can't run commands.
    fn run(&self, _cmd: &str, _tx: Sender<Msg>) -> bool {
        false
    }
    /// Whether [`Host::compress`] would try `bytes`. Cheap.
    fn compressible(&self, bytes: &[u8]) -> bool {
        crate::images::compressible(bytes)
    }
    /// Compresses images for `page` in the background, each to lossy WebP when that's smaller,
    /// renamed to `.webp`. Sends [`Msg::Images`].
    fn compress(&self, page: PathBuf, images: Vec<(String, Vec<u8>)>, tx: Sender<Msg>) {
        std::thread::spawn(move || {
            let images = images
                .into_iter()
                .map(|(name, bytes)| match crate::images::compress(&bytes) {
                    Some(webp) => (crate::paste::webp_name(&name), webp),
                    None => (name, bytes),
                })
                .collect();
            let _ = tx.send(Msg::Images(page, images));
        });
    }
}

/// The local git CLI on a directory.
#[cfg(feature = "native")]
pub struct GitHost {
    pub root: std::path::PathBuf,
}

#[cfg(feature = "native")]
impl Host for GitHost {
    fn fetch(&self, tx: Sender<Msg>) {
        let root = self.root.clone();
        std::thread::spawn(move || {
            let (state, pulled) = crate::git::fetch_and_fast_forward(&root);
            let _ = tx.send(Msg::Fetched(state, pulled));
        });
    }

    fn sync(&self, commit_prefix: &str, tx: Sender<Msg>) {
        let root = self.root.clone();
        let prefix = commit_prefix.to_string();
        std::thread::spawn(move || {
            let res = crate::git::sync(&root, &prefix).map_err(|e| e.to_string());
            let _ = tx.send(Msg::Synced(res, crate::git::state(&root)));
        });
    }

    fn changes(&self) -> usize {
        crate::git::changes(&self.root)
    }

    fn open(&self, target: Open) -> Result<()> {
        match target {
            Open::Url(u) => open::that_detached(u)?,
            Open::File(p) => {
                let path = self.root.join(p);
                if runs_code(&path) {
                    anyhow::bail!("{} could run code, so gition won't open it", crate::paths::to_slash(p));
                }
                open::that_detached(path)?
            }
        }
        Ok(())
    }

    fn copy(&self, text: &str) -> bool {
        // The local clipboard tool when there is one; over SSH it would be the wrong machine.
        if local() && clipboard_tool(text) {
            return true;
        }
        osc52(text).is_ok()
    }

    fn fetch_title(&self, url: &str, tx: Sender<Msg>) {
        let url = url.to_string();
        std::thread::spawn(move || {
            let title = page_title(&url);
            let _ = tx.send(Msg::Title(url, title));
        });
    }

    fn paste_image(&self) -> Option<Vec<u8>> {
        local().then(clipboard_image).flatten()
    }

    fn run(&self, cmd: &str, tx: Sender<Msg>) -> bool {
        let root = self.root.clone();
        let cmd = cmd.to_string();
        std::thread::spawn(move || {
            let res = run_shell(&root, &cmd);
            let _ = tx.send(Msg::Ran(cmd, res, crate::git::state(&root)));
        });
        true
    }
}

/// Whether the OS opener would run `path` rather than show it: app bundles and other directories,
/// scripts and executables. A cloned repo can hold these and link to them from a page.
#[cfg(feature = "native")]
fn runs_code(path: &Path) -> bool {
    const EXECUTABLE: &[&str] = &[
        "app", "appimage", "bat", "cmd", "com", "command", "cpl", "desktop", "exe", "hta", "jar", "js", "jse", "lnk",
        "msi", "pif", "pkg", "ps1", "scpt", "scr", "sh", "terminal", "tool", "url", "vbe", "vbs", "workflow", "ws",
        "wsf",
    ];
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if meta.is_dir() {
        return true;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => EXECUTABLE.iter().any(|x| ext.eq_ignore_ascii_case(x)),
        None => is_executable(&meta),
    }
}

#[cfg(all(feature = "native", unix))]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(all(feature = "native", not(unix)))]
fn is_executable(_: &std::fs::Metadata) -> bool {
    false
}

/// Runs `cmd` in the system shell with no input; stdout and stderr together.
#[cfg(feature = "native")]
fn run_shell(root: &Path, cmd: &str) -> Result<String, String> {
    use std::process::{Command, Stdio};
    let (shell, flag) = if cfg!(windows) { ("cmd", "/C") } else { ("sh", "-c") };
    let out = Command::new(shell)
        .args([flag, cmd])
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let text = text.trim_end().to_string();
    if out.status.success() {
        Ok(text)
    } else if text.is_empty() {
        Err(out.status.to_string())
    } else {
        Err(text)
    }
}

/// Not over SSH, where the clipboard tools would reach the wrong machine.
#[cfg(feature = "native")]
fn local() -> bool {
    std::env::var_os("SSH_CONNECTION").is_none() && std::env::var_os("SSH_TTY").is_none()
}

/// Fetches `url` and reads its title. None on any error, or if it isn't HTML.
#[cfg(feature = "native")]
fn page_title(url: &str) -> Option<String> {
    use std::io::Read;
    use std::time::Duration;

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .max_redirects(5)
        .user_agent(concat!("gition/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut res = agent.get(url).call().ok()?;
    let html = res.headers().get("content-type")?.to_str().ok()?.to_ascii_lowercase().contains("text/html");
    if !html {
        return None;
    }
    let mut body = Vec::new();
    res.body_mut().as_reader().take(512 * 1024).read_to_end(&mut body).ok()?;
    crate::paste::page_title(&String::from_utf8_lossy(&body))
}

/// The clipboard's image as PNG, through the platform's clipboard command.
#[cfg(feature = "native")]
fn clipboard_image() -> Option<Vec<u8>> {
    use std::process::{Command, Stdio};

    let run = |cmd: &str, args: &[&str]| {
        let out = Command::new(cmd).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        out.status.success().then_some(out.stdout)
    };
    let png = |b: &Vec<u8>| b.starts_with(b"\x89PNG");
    if cfg!(target_os = "macos") {
        // pngpaste if it's installed, otherwise AppleScript prints «data PNGf89504E…».
        run("pngpaste", &["-"]).filter(png).or_else(|| {
            let out = run("osascript", &["-e", "the clipboard as «class PNGf»"])?;
            let hex = String::from_utf8(out).ok()?;
            let hex = hex.trim().strip_prefix("«data PNGf")?.strip_suffix('»')?;
            let bytes: Option<Vec<u8>> =
                (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok()).collect();
            bytes.filter(png)
        })
    } else if cfg!(windows) {
        None
    } else {
        run("wl-paste", &["--type", "image/png"])
            .filter(png)
            .or_else(|| run("xclip", &["-selection", "clipboard", "-target", "image/png", "-out"]).filter(png))
    }
}

/// Pipes `text` into the platform's clipboard command.
#[cfg(feature = "native")]
fn clipboard_tool(text: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let tools: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else if cfg!(windows) {
        &[("clip", &[])]
    } else {
        &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard"]), ("xsel", &["--clipboard", "--input"])]
    };
    tools.iter().any(|(cmd, args)| {
        let Ok(mut child) = Command::new(cmd).args(*args).stdin(Stdio::piped()).stderr(Stdio::null()).spawn() else {
            return false;
        };
        let wrote = child.stdin.take().is_some_and(|mut s| s.write_all(text.as_bytes()).is_ok());
        child.wait().is_ok_and(|s| s.success()) && wrote
    })
}

/// Asks the terminal to set the clipboard (OSC 52). Inside tmux the sequence is passed through.
#[cfg(feature = "native")]
fn osc52(text: &str) -> std::io::Result<()> {
    use std::io::Write;

    let seq = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let seq = if std::env::var_os("TMUX").is_some() {
        format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b"))
    } else {
        seq
    };
    let mut out = std::io::stdout();
    out.write_all(seq.as_bytes())?;
    out.flush()
}

#[cfg(feature = "native")]
fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ABC[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

#[cfg(all(test, feature = "native"))]
mod tests {
    #[test]
    fn base64_pads() {
        assert_eq!(super::base64(b"ab"), "YWI=");
        assert_eq!(super::base64(b"abc"), "YWJj");
        assert_eq!(super::base64(b"a"), "YQ==");
        assert_eq!(super::base64("ğ".as_bytes()), "xJ8=");
    }
}
