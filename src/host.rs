//! What the app needs from its environment besides files: syncing and
//! opening things outside gition. The terminal build uses the git CLI, the web
//! build the GitHub API.

use std::path::Path;
use std::sync::mpsc::Sender;

use anyhow::Result;

use crate::sync::State;

/// Results of background work, applied in `App::tick`.
pub enum Msg {
    /// Fetch finished; the number of commits fast-forwarded.
    Fetched(State, usize),
    Synced(Result<String, String>, State),
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
            Open::File(p) => open::that_detached(self.root.join(p))?,
        }
        Ok(())
    }
}
