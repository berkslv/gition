# Changelog

## 0.2.0

### Added
- Pasted and inserted PNG/JPEG images are saved as WebP when that's smaller (`compress_images`, on by default).
- Switching between the viewer and the editor keeps your place on the page.
- The status bar shows how many files changed since the last sync.

### Security
- Links only open over `http`, `https` and `mailto`. A `javascript:` link in a repo could run in the web app and read the GitHub token.
- Following a link to a script, app bundle or other executable no longer runs it.
- Files are never read or written through symlinks that point outside the repo.
- `assets_dir` in `.gition.toml` must stay inside the repo.
- Image decoding has size and memory limits.
- Web: repository names from the URL are validated, and sign-in sessions expire on the server. Existing sessions sign in again once.

### Performance
- The screen is redrawn only when something changed, so an idle gition uses no CPU (terminal and web).
- Page layout is cached instead of rebuilt on every frame.
- Decoded images are shrunk to terminal size and the cache is capped.
- Web: faster file tree and saves in large repositories.
- Smaller, faster release binaries (thin LTO, stripped).

## 0.1.1
- Themes, update check, titled links and starter files.

## 0.1.0
- First release: a terminal notebook on markdown and git.
