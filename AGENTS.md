# AGENTS.md

gition is a Rust TUI notebook: a markdown page tree, a viewer and an editor, with git as storage.

## Commands

- `cargo test`: unit tests, plus integration tests in `tests/`. The integration tests need `git` on PATH.
- `cargo clippy --all-targets` and `cargo fmt`. Formatting uses `rustfmt.toml` (width 120).
- UI snapshots live in `tests/snapshots/`. Review changes with `cargo insta review`, or regenerate them with `INSTA_UPDATE=always cargo test`.

## Layout

- `main.rs`: arguments, repo discovery and the init prompt, the terminal loop.
- `app.rs`: all state and key handling. Actions come from the keymap; keys are never hardcoded.
- `ui.rs`: drawing only. Must not mutate files.
- `markdown.rs`: pulldown-cmark to styled `Block`s, plus `wrap`.
- `links.rs`: finding and rewriting inline links. Used by rename/move and the delete warning.
- `git.rs`: shells out to the `git` CLI. Don't use `git2`/`gix`.
- `tree.rs`, `paths.rs`, `repo.rs`, `config.rs`, `keymap.rs`, `images.rs`.

## Rules

- Paths inside the app are repo-relative `PathBuf`s. Use `paths::to_slash` for links and display.
- The repo marker is exactly `INDEX.md`, uppercase.
- Only the sync action (`Ctrl+G`) commits. File operations never commit.
- Keep each file's existing line endings. New files use LF.
- To add a key, add an entry to `actions!` in `keymap.rs` and handle it in the matching context list in `app.rs`. The help overlay reads from the keymap.
- Keep the keymap small and simple. That's the point of the product.
