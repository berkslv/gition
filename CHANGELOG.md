# Changelog

## 0.3.0

### Added
- Web: repos open right away. Trees and files are cached in the browser, so a repo you've opened before loads from the cache without waiting for GitHub. Unsynced changes stay saved too.
- Web: the first screen loads first, and the rest of the notebook downloads in the background. The status bar shows `loading pages N/M` until it's done. Opening a page that hasn't downloaded yet shows "Loading…" until it arrives.
- Web: the gition logo is the app icon, also when it's added to the iOS home screen.

### Changed
- Moving, renaming and deleting pages wait until every page has downloaded, because they read every page to update or count links.

## 0.2.2

### Changed
- CI runs only on release tags, and each release deploys the web app. Pushes to main no longer deploy it.

## 0.2.1

### Added
- Web: images show inline in pages, like in the terminal.
- Any git repo opens as a notebook; an `INDEX.md` is no longer required. The root `INDEX.md` gets its own row in the tree, and new pages and folders work in an empty tree.
- Web: Ctrl+C or quit goes back to the repo list.

### Fixed
- Web: dialogs drew off-center and the page was cut off at the right and bottom when the tab loaded in the background. The grid now rebuilds whenever it doesn't fit the page.

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
