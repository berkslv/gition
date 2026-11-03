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
- `i` opens a file picker, and `o` opens the file on GitHub. Images show as placeholders.
- Touch and mouse work alongside the keys: tap a page to open it or a link to follow it, double tap to edit, drag or scroll to move, swipe right or left to show or hide the pages. Tap the status bar (☰ Menu), long press or right click for a menu of actions. On phones the keyboard comes up only while there's something to type.
- Privacy: your notes go straight from your browser to GitHub. The Worker only handles sign-in and keeps nothing: your GitHub tokens live in an encrypted cookie in your browser. gition can only reach the repos you install the [gition-notes](https://github.com/apps/gition-notes) App on, and you can remove it any time in your GitHub settings.

## Keys

| Where  | Keys |
|--------|------|
| Global | `Tab` switch focus · `/` find page · `Ctrl+G` sync · `Ctrl+B` toggle tree · `?` help · `q`/`Ctrl+C` quit |
| Tree   | `j/k` move · `l`/`Enter` open · `h` collapse · `n` new page · `N` new folder · `r` rename/move · `d` delete · `i` insert image · `.` show all files |
| Page   | `j/k` move · `PgUp/PgDn` · `e`/`Enter` edit · `Space` follow link · `o` open externally |
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
