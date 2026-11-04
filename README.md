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

```sh
curl -fsSL https://raw.githubusercontent.com/berkslv/gition/main/scripts/install.sh | sh      # macOS / Linux
powershell -c "irm https://raw.githubusercontent.com/berkslv/gition/main/scripts/install.ps1 | iex"   # Windows
```

Or `cargo install gition`, or grab a binary from [Releases](https://github.com/berkslv/gition/releases). Requires `git` on your PATH. gition tells you when an update is out and prints the command on quit (`GITION_NO_UPDATE_CHECK=1` turns this off).

## Use

```sh
cd ~/notes && gition      # or: gition ~/notes
```

gition opens the nearest folder with an `INDEX.md`, or offers to set one up (git repo, `INDEX.md`, `assets/`, config and a first commit).

- `a.md` is a page; a folder is a page when it has `a/INDEX.md`.
- Images go into `assets/` with relative links, so they render on GitHub too. Paste or drop an image path, or press `Ctrl+V` for a clipboard image.
- A pasted URL becomes a link titled after its page.
- Renaming or moving a page rewrites every link to and from it.
- `Ctrl+G` commits, pulls with rebase and pushes. File operations never commit on their own.
- The mouse works: click to open or follow, double click to edit, drag to select and copy, right click for a menu.

**Import from Notion:** export as "Markdown & CSV", unzip, then run `gition ~/notes import notion ~/Downloads/notion-export`. Subpages become folders, databases become linked tables (or plain ones with `--database-as-table`), and links are rewritten. Nothing is overwritten or committed.

## Web

The same app runs in the browser on a GitHub repo, with no clone. Sign in, install the [gition-notes](https://github.com/apps/gition-notes) App on the repos you want, and pick one (`/#owner/repo` opens one directly). Changes are kept in the browser until `Ctrl+G` commits them through the GitHub API; when both sides changed a file, yours is saved as `name.conflict.md`. It works on phones too, with touch gestures and a tap menu.

Your notes go straight from the browser to GitHub. The server only handles sign-in and link titles, and stores nothing.

## Keys

| Where  | Keys |
|--------|------|
| Global | `Tab` switch focus · `/` find page · `!` shell command · `Ctrl+G` sync · `Ctrl+B` toggle tree · `?` help · `q` quit |
| Tree   | `j/k` move · `l`/`Enter` open · `h` collapse · `n` new page · `N` new folder · `r` rename/move · `d` delete · `i` insert image · `.` all files |
| Page   | `j/k` move · `PgUp/PgDn` · `e`/`Enter` edit · `Space` follow link · `o` open externally |
| Editor | `Ctrl+S` save · `Ctrl+A` select all · `Ctrl+C` copy · `Ctrl+X` cut line · `Ctrl+Z/Y` undo/redo · `Ctrl+V` paste image · `Esc` back |

On macOS, Cmd works as Ctrl in the browser and in terminals with the kitty keyboard protocol.

## Config

`.gition.toml` at the repo root (optional, committed with the notes):

```toml
tree_width = 30
assets_dir = "assets"
show_all_files = false
commit_prefix = "gition:"
link_titles = true       # a pasted URL becomes [Page title](url)
compress_images = true   # pasted PNG/JPEG images are saved as WebP

[keys]
quit = ["q", "ctrl+q"]   # action names are in src/keymap.rs

[theme]
preset = "nord"          # default, light, nord, gruvbox, dracula or catppuccin
link = "#8fbcbb"         # a color name, #rrggbb or a 256-color index
```

## License

MIT
