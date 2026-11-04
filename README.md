# gition

A Notion-style notebook for the terminal. Pages are plain markdown files in a git repo.

```
┌ Pages ─────────────┐┌ Projects/plan.md ───────────────────────┐
│▾ notes             ││Plan                                     │
│▾ Projects          ││                                         │
│    plan            ││• ship v1                                │
│  ideas             ││diagram, photo.                          │
└────────────────────┘└─────────────────────────────────────────┘
```

## Install

macOS / Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/berkslv/gition/main/scripts/install.sh | sh
```

Windows:

```powershell
powershell -c "irm https://raw.githubusercontent.com/berkslv/gition/main/scripts/install.ps1 | iex"
```

Or `cargo install gition`, or download a binary from [Releases](https://github.com/berkslv/gition/releases). The scripts accept `GITION_VERSION` and `GITION_INSTALL_DIR`.

On startup gition checks GitHub for a newer release in the background. If there is one, it says so, and prints the update command when you quit:

```sh
curl -fsSL https://raw.githubusercontent.com/berkslv/gition/main/scripts/update.sh | sh
```

```powershell
powershell -c "irm https://raw.githubusercontent.com/berkslv/gition/main/scripts/update.ps1 | iex"
```

The update scripts replace gition where it's installed now. Set `GITION_NO_UPDATE_CHECK=1` to turn the check off.

Requires `git` on your PATH.

## Use

```sh
cd ~/notes && gition      # or: gition ~/notes
```

gition walks up from the current directory to the nearest `INDEX.md`. If there isn't one, it offers to initialize the repo: `git init`, `INDEX.md`, `assets/`, `.gitattributes`, `.gition.toml`, an `AGENTS.md` that tells coding agents how the notebook is laid out (hidden from the page tree), and a first commit.

- `a.md` is a page. A folder is a page when it has an `a/INDEX.md`.
- Images are copied into `assets/` and linked with relative paths, so they also render on GitHub.
- Pasting a bare URL in the editor turns it into a link named after the page, like `[Rust Programming Language](https://www.rust-lang.org)`, once the title arrives. Undo brings back the bare URL. Sites that block bots, and pages that can't be read, keep the bare URL.
- Pasting an image's path, or dropping the file into the terminal, copies the image into `assets/` and links it. `Ctrl+V` in the editor pastes the image on the clipboard as `assets/pasted-YYYYMMDD-HHMMSS.png` (UTC). That needs `pngpaste` or `osascript` on macOS, `wl-paste` or `xclip` on Linux, and doesn't work over SSH. Cmd+V does the same in terminals that send an empty paste when the clipboard only holds an image; others send nothing, which is why it's its own key.
- Moving or renaming a page rewrites every relative link to and from it.
- `Ctrl+G` saves, runs `git add -A`, commits with a message listing the changed files, then pulls with `--rebase` and pushes. On startup gition fetches, and fast-forwards when that's safe.
- The mouse works too: click to open a page, follow a link or place the cursor, double click to edit, scroll with the wheel, right click for a menu. Drag to select text; it's copied when you let go, through the system clipboard tool or, over SSH, the terminal (OSC 52). Hold Shift (Option in iTerm2 and Terminal.app) for the terminal's own selection.

### Import from Notion

In Notion, export your pages as "Markdown & CSV" and unzip the export. Then run:

```sh
gition ~/notes import notion ~/Downloads/notion-export
```

The pages are copied into `~/notes`, or into the folder you name. A page with subpages becomes a folder with an `INDEX.md`. A database becomes a page with a table that links to its rows, and each row's properties become front matter at the top of its page. Attachments go into `assets/`, and links between pages are rewritten to the new paths. With `--database-as-table` (`gition ~/notes import notion --database-as-table EXPORT`), a database becomes a page with a plain table, with no page per row. Nothing is overwritten: a title that's already taken gets a number. Nothing is committed either; press `Ctrl+G` in gition to sync.

## Web

The same app also runs in the browser on a GitHub repo, with no local clone. It's in `web/` and deploys to Cloudflare Workers.

- Sign in with GitHub, install the gition GitHub App on the repos you want, and pick one. Open `/#owner/repo` to go straight to a repo, or `/#repos` to switch.
- `Ctrl+S` saves to the browser's working copy, which survives reloads. `Ctrl+G` commits every change in one commit through the GitHub API. If someone pushed in between, gition replays your changes on top of theirs. When both sides changed the same file, it keeps their version and saves yours next to it as `name.conflict.md`.
- `i` opens a file picker, and `o` opens the file on GitHub. Images show as placeholders. Pasting an image (the browser's own paste) puts it in `assets/`, and pasted URLs become titled links as in the terminal.
- Touch and mouse work alongside the keys: tap a page to open it or a link to follow it, double tap to edit, drag or scroll to move (with a mouse, drag to select and copy), swipe right or left to show or hide the pages. Tap the status bar (☰ Menu), long press or right click for a menu of actions. On phones the keyboard comes up only while there's something to type.
- Privacy: your notes go straight from your browser to GitHub. The Worker only handles sign-in and reading the titles of pages you paste links to, and keeps nothing: your GitHub tokens live in an encrypted cookie in your browser. gition can only reach the repos you install the [gition-notes](https://github.com/apps/gition-notes) App on, and you can remove it any time in your GitHub settings.

## Keys

| Where  | Keys |
|--------|------|
| Global | `Tab` switch focus · `/` find page · `!` run a shell command · `Ctrl+G` sync · `Ctrl+B` toggle tree · `?` help · `q`/`Ctrl+C` quit |
| Tree   | `j/k` move · `l`/`Enter` open · `h` collapse · `n` new page · `N` new folder · `r` rename/move · `d` delete · `i` insert image · `.` show all files |
| Page   | `j/k` move · `PgUp/PgDn` · `e`/`Enter` edit · `Space` follow link · `o` open externally |
| Editor | type normally · `Ctrl+S` save · `Ctrl+A` select all · `Ctrl+C` copy selection · `Ctrl+X` cut line · `Ctrl+Z/Y` undo/redo · `Ctrl+V` paste image · `Esc` back |

On macOS, Cmd works in place of Ctrl in the browser, and in terminals that report Cmd through the kitty keyboard protocol: kitty, and WezTerm with `config.enable_kitty_keyboard = true`. Cmd+V pastes text through the terminal as usual. Cmd+C stays the terminal's copy in most terminals; a mouse drag in gition copies on release anyway. In Terminal.app and iTerm2, use Ctrl.

## Config

`.gition.toml` at the repo root, created by init and optional. It is committed, so it's shared with everyone who uses the repo:

```toml
tree_width = 30
assets_dir = "assets"
show_all_files = false
commit_prefix = "gition:"
link_titles = true   # a pasted URL becomes [Page title](url)
compress_images = true   # pasted PNG/JPEG images are saved as WebP

[keys]
quit = ["q", "ctrl+q"]
sync = "ctrl+g"

[theme]
preset = "nord"      # default, light, nord, gruvbox, dracula or catppuccin
link = "#8fbcbb"     # any color overrides the preset's
```

Action names are listed in `src/keymap.rs`. A color is a name (`cyan`, `dark_gray`), `#rrggbb`, or a 256-color index like `"236"`. The theme colors are `accent`, `dim`, `cursor`, `bar`, `selection`, `folder`, `current`, `ok`, `warning`, `error`, `heading1`–`heading3`, `link`, `code`, `bullet` and `image`. `default` and `light` use your terminal's own colors; the other presets need a terminal with true color.

## License

MIT
