# AGENTS.md

gition is a Rust TUI notebook: a markdown page tree, a viewer and an editor, with git as storage.

## Commands

- `cargo test`: unit tests, plus integration tests in `tests/`. The integration tests need `git` on PATH.
- `cargo clippy --all-targets` and `cargo fmt`. Formatting uses `rustfmt.toml` (width 120).
- `cargo clippy --lib --no-default-features --target wasm32-unknown-unknown` checks that the shared code builds for the web.
- In `web/`: `cargo clippy --target wasm32-unknown-unknown --all-targets`. To run it locally, build with `trunk build --release` in `web/client` and `worker-build --release` in `web/server`, then run `npx wrangler dev` in `web/`. Restart `wrangler dev` after each rebuild, because it snapshots the asset list. This needs `cargo install trunk worker-build` and a `web/.dev.vars` with the secrets.
- UI snapshots live in `tests/snapshots/`. Review changes with `cargo insta review`, or regenerate them with `INSTA_UPDATE=always cargo test`.

## Releasing

Bump `version` in `Cargo.toml`, commit, then `git tag -a vX.Y.Z -m vX.Y.Z && git push origin main vX.Y.Z`.
CI runs only on these tags, not on pushes to main or on PRs, so run the tests locally before tagging. On the tag, `.github/workflows/ci.yml` checks that the tag matches the version, runs the tests, builds the binaries, creates the GitHub Release, publishes to crates.io through Trusted Publishing, checks the install scripts against the new release and deploys the web app. Never run `cargo publish` by hand.

## Web deploy

`.github/workflows/ci.yml` deploys `web/` with `wrangler deploy` on every release tag, after the tests and the web build pass. It uses the repo secrets `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`. The Worker needs the vars `GITHUB_CLIENT_ID` and `GITHUB_APP_SLUG` in `web/wrangler.toml`, plus the secrets `GITHUB_CLIENT_SECRET` and `SESSION_KEY` (`npx wrangler secret put NAME`).

## Layout

`src/` is shared by the terminal app and the web app. The `native` feature (on by default) adds the terminal frontend, the git CLI and inline images. Without it, the library builds for `wasm32`.

- `main.rs`: arguments, repo discovery and the init prompt, the terminal loop.
- `app.rs`: all state and key handling. Actions come from the keymap; keys are never hardcoded. Repo files go through `Storage` and sync goes through `Host`, never `std::fs` or git directly.
- `storage.rs`: the `Storage` trait. `FsStorage` is a directory on disk; `MemStorage` is the web's in-memory working copy.
- `host.rs`: the `Host` trait (fetch, sync, open externally, file picker, image compression). `GitHost` is the git CLI; image compression defaults to libwebp on a thread, and `WebHost` uses the browser's encoder. `sync.rs` holds the sync state, commit messages, and diff/rebase on file snapshots.
- `ui.rs`: drawing only. Must not mutate files.
- `pointer.rs`: touch and mouse. `ui.rs` records what it drew where (`App::hits`); taps, long presses, scrolls and swipes run the same actions as keys. A mouse drag selects text and copies it on release through `Host::copy`. The touch menu lists actions; it has no key of its own.
- `markdown.rs`: pulldown-cmark to styled `Block`s, plus `wrap`.
- `links.rs`: finding and rewriting inline links. Used by rename/move and the delete warning.
- `paste.rs`: what a paste turns into: bare URL detection, page titles from HTML, image paths from a terminal paste, names for clipboard images. Pure, so the worker uses it too. The app swaps a pasted URL for its link when `Host::fetch_title` answers with `Msg::Title`.
- `import.rs`: `gition [PATH] import notion EXPORT_DIR`. Turns a Notion "Markdown & CSV" export into pages, through `Storage`.
- `git.rs`: shells out to the `git` CLI. Don't use `git2`/`gix`.
- `update.rs`: the startup check for a newer GitHub release. `main.rs` shows it while gition runs and prints the update command on quit.
- `scripts/`: install and update scripts, `.sh` for macOS/Linux and `.ps1` for Windows. `update.rs` prints the update script's URL, so keep the two in step.
- `tree.rs`, `paths.rs`, `repo.rs`, `config.rs`, `keymap.rs`, `images.rs`.
- `web/client`: the browser app (Ratzilla). `github.rs` is the API client, `workspace.rs` holds the working copy and `WebHost`, `pictures.rs` shows inline images as `<img>`s under transparent slots the viewer leaves in the grid (`images::SLOT`), and `main.rs` handles sign-in, the repo picker, keyboard input and pointer gestures. It drives its own frame loop because Ratzilla's `DomBackend::size()` is wrong on phones, and rebuilds the grid whenever it doesn't fit the page.
- `web/server`: the Worker. It handles GitHub sign-in (`/auth/*`) and keeps tokens in an encrypted cookie. `/auth/title` reads a page's title for signed-in users, since the browser can't across origins.

## Rules

- Paths inside the app are repo-relative `PathBuf`s. Use `paths::to_slash` for links and display.
- `INDEX.md` is optional. When the root has one (exactly this case), it is the home page; otherwise the notebook is the git root. A folder is a page when it holds `INDEX.md`.
- Only the sync action (`Ctrl+G`) commits. File operations never commit.
- Keep each file's existing line endings. New files use LF.
- To add a key, add an entry to `actions!` in `keymap.rs` and handle it in the matching context list in `app.rs`. The help overlay reads from the keymap.
- Keep the keymap small and simple. That's the point of the product.
