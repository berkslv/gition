# gition

A Notion-style notebook for the terminal. Pages are plain markdown files in a git repo.

```
┌ Pages ─────────────┐┌ Projects/plan.md ───────────────────────┐
│▾ notes             ││Plan                                     │
│▾ Projects          ││                                         │
│    plan            ││• ship v1                                │
│  ideas             ││🖼 diagram                                │
└────────────────────┘└─────────────────────────────────────────┘
```

## Install

macOS / Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/berkslv/gition/main/install.sh | sh
```

Windows:

```powershell
powershell -c "irm https://raw.githubusercontent.com/berkslv/gition/main/install.ps1 | iex"
```

Or `cargo install gition`, or download a binary from [Releases](https://github.com/berkslv/gition/releases). The scripts accept `GITION_VERSION` and `GITION_INSTALL_DIR`.

Requires `git` on your PATH.

## Use

```sh
cd ~/notes && gition      # or: gition ~/notes
```

gition walks up from the current directory to the nearest `INDEX.md`. If there isn't one, it offers to initialize the repo: `git init`, `INDEX.md`, `assets/`, `.gitattributes`, and a first commit.

- `a.md` is a page. A folder is a page when it has an `a/INDEX.md`.
- Images are copied into `assets/` and linked with relative paths, so they also render on GitHub.
- Moving or renaming a page rewrites every relative link to and from it.
- `Ctrl+G` saves, runs `git add -A`, commits with a message listing the changed files, then pulls with `--rebase` and pushes. On startup gition fetches, and fast-forwards when that's safe.

## Keys

| Where  | Keys |
|--------|------|
| Global | `Tab` switch focus · `/` find page · `Ctrl+G` sync · `Ctrl+B` toggle tree · `?` help · `q` quit |
| Tree   | `j/k` move · `l`/`Enter` open · `h` collapse · `n` new page · `N` new folder · `r` rename/move · `d` delete · `i` insert image · `.` show all files |
| Page   | `j/k` move · `PgUp/PgDn` · `e` edit · `Enter` follow link · `o` open externally |
| Editor | type normally · `Ctrl+S` save · `Ctrl+Z/Y` undo/redo · `Esc` back |

## Config

Optional `.gition.toml` at the repo root. It is committed, so it's shared with everyone who uses the repo:

```toml
tree_width = 30
assets_dir = "assets"
show_all_files = false
commit_prefix = "gition:"

[keys]
quit = ["q", "ctrl+q"]
sync = "ctrl+g"
```

Action names are listed in `src/keymap.rs`.

## License

MIT
