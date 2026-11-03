//! Importing a Notion export ("Markdown & CSV", unzipped) into a gition repo.
//!
//! Notion names each page `Title <32 hex id>.md` and keeps its subpages and attachments in a
//! folder `Title/` next to it. A database is `Title <id>.csv` (plus `_all.csv`), with its rows
//! as pages in `Title/`. In gition a page with subpages becomes `Title/INDEX.md`, any other
//! page `Title.md`, a database a page with a table, and attachments go under the assets folder.
//! The properties of a database's rows become front matter. Links are rewritten to the new paths.
//!
//! With [`Options::databases_as_tables`], a database is a page `Title.md` with a plain table,
//! without row pages.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::links;
use crate::paths;
use crate::repo::INDEX;
use crate::storage::Storage;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub pages: usize,
    pub databases: usize,
    pub files: usize,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Options {
    /// Import a database as a page with a plain table, without row pages.
    pub databases_as_tables: bool,
}

/// Imports the export in `src` into the folder `into` of the repo in `dest`. Never overwrites:
/// a title that's taken gets a number, as in `Notes 2`.
pub fn notion(src: &dyn Storage, dest: &dyn Storage, into: &Path, assets_dir: &str, opts: Options) -> Result<Summary> {
    let mut plan =
        Plan { src, dest, opts, jobs: Vec::new(), map: HashMap::new(), taken: HashMap::new(), rows: HashMap::new() };
    plan.dir(Path::new(""), into, &Path::new(assets_dir).join(into))?;
    if plan.jobs.is_empty() {
        bail!("no Notion pages found; export as \"Markdown & CSV\" and unzip it first");
    }
    if let Some(job) = plan.jobs.iter().find(|j| dest.exists(j.dest())) {
        bail!("{} already exists", paths::to_slash(job.dest()));
    }
    plan.write()
}

enum Job {
    Page { src: PathBuf, dest: PathBuf },
    Database { csv: PathBuf, title: String, rows: Option<PathBuf>, dest: PathBuf },
    File { src: PathBuf, dest: PathBuf },
}

impl Job {
    fn dest(&self) -> &Path {
        match self {
            Job::Page { dest, .. } | Job::Database { dest, .. } | Job::File { dest, .. } => dest,
        }
    }
}

struct Plan<'a> {
    src: &'a dyn Storage,
    dest: &'a dyn Storage,
    opts: Options,
    jobs: Vec<Job>,
    /// Export path -> repo path, for rewriting links.
    map: HashMap<PathBuf, PathBuf>,
    /// Lowercased names used in each repo folder, as file systems may ignore case.
    taken: HashMap<PathBuf, HashSet<String>>,
    /// Export folders holding the rows of a database -> its column names.
    rows: HashMap<PathBuf, Vec<String>>,
}

impl Plan<'_> {
    /// Plans the export folder `sdir` into the repo folder `ddir`. Loose files in it are
    /// attachments of the page that owns the folder and go to `assets`.
    fn dir(&mut self, sdir: &Path, ddir: &Path, assets: &Path) -> Result<()> {
        let mut entries = self.read_dir(sdir)?;
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let dirs: HashSet<&str> = entries.iter().filter(|e| e.is_dir).map(|e| e.name.as_str()).collect();
        let mut owned: HashSet<String> = HashSet::new();
        let mut databases: Vec<(String, PathBuf, Vec<PathBuf>)> = Vec::new();

        for e in entries.iter().filter(|e| !e.is_dir) {
            let path = sdir.join(&e.name);
            match split_id(&e.name) {
                Some((title, "md", _)) => {
                    let child = sdir.join(title);
                    let has_child = dirs.contains(title) && owned.insert(title.to_string());
                    let folder = has_child && self.has_pages(&child)?;
                    let title = self.full_title(&path, title);
                    let name = self.claim(ddir, &title);
                    let dest = if folder { ddir.join(&name).join(INDEX) } else { ddir.join(format!("{name}.md")) };
                    self.map.insert(path.clone(), dest.clone());
                    self.jobs.push(Job::Page { src: path, dest });
                    let assets = assets.join(&name);
                    if folder {
                        self.dir(&child, &ddir.join(&name), &assets)?;
                    } else if has_child {
                        self.files(&child, &assets)?;
                    }
                }
                Some((title, _, all)) => match databases.iter_mut().find(|d| d.0 == title) {
                    // Prefer `_all.csv`: the plain one only has the rows of the saved view.
                    Some(d) => {
                        if all {
                            d.1 = path.clone();
                        }
                        d.2.push(path);
                    }
                    None => databases.push((title.to_string(), path.clone(), vec![path])),
                },
                None => self.file(path, assets.join(&e.name)),
            }
        }

        for (title, csv, sources) in databases {
            let name = self.claim(ddir, &title);
            if self.opts.databases_as_tables {
                // Without row pages, the folder of rows is left out entirely.
                owned.insert(title.clone());
                let dest = ddir.join(format!("{name}.md"));
                for s in sources {
                    self.map.insert(s, dest.clone());
                }
                self.jobs.push(Job::Database { csv, title, rows: None, dest });
                continue;
            }
            let folder = ddir.join(&name);
            let dest = folder.join(INDEX);
            for s in sources {
                self.map.insert(s, dest.clone());
            }
            let rows = (dirs.contains(title.as_str()) && owned.insert(title.clone())).then(|| sdir.join(&title));
            if let Some(rows) = &rows {
                let columns = parse_csv(&self.read_text(&csv)?).into_iter().next().unwrap_or_default();
                self.rows.insert(rows.clone(), columns);
                self.dir(rows, &folder, &assets.join(&name))?;
            }
            self.jobs.push(Job::Database { csv, title, rows, dest });
        }

        for d in dirs.iter().filter(|d| !owned.contains(**d)) {
            self.files(&sdir.join(d), &assets.join(d))?;
        }
        Ok(())
    }

    /// Plans everything under `sdir` as attachments in `assets`.
    fn files(&mut self, sdir: &Path, assets: &Path) -> Result<()> {
        for e in self.read_dir(sdir)? {
            let path = sdir.join(&e.name);
            if e.is_dir {
                self.files(&path, &assets.join(&e.name))?;
            } else {
                self.file(path, assets.join(&e.name));
            }
        }
        Ok(())
    }

    fn file(&mut self, src: PathBuf, dest: PathBuf) {
        self.map.insert(src.clone(), dest.clone());
        self.jobs.push(Job::File { src, dest });
    }

    fn read_dir(&self, dir: &Path) -> Result<Vec<crate::storage::DirEntry>> {
        let entries = self.src.read_dir(dir).with_context(|| format!("reading {}", paths::to_slash(dir)))?;
        Ok(entries.into_iter().filter(|e| !e.name.starts_with('.')).collect())
    }

    /// True if the export folder holds subpages or databases, not only attachments.
    fn has_pages(&self, dir: &Path) -> Result<bool> {
        Ok(self.read_dir(dir)?.iter().any(|e| !e.is_dir && split_id(&e.name).is_some()))
    }

    /// Notion cuts titles in file names at 50 characters; the page's heading has all of it.
    fn full_title(&self, page: &Path, title: &str) -> String {
        if title.chars().count() < 50 {
            return title.to_string();
        }
        let text = self.read_text(page).unwrap_or_default();
        let Some(heading) = text.lines().next().and_then(|l| l.strip_prefix("# ")) else { return title.to_string() };
        let name: String =
            heading.chars().filter(|&c| !r#"/\:*?"<>|"#.contains(c) && !c.is_control()).take(100).collect();
        let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
        let name = name.trim_matches('.');
        if plain(name).starts_with(&plain(title)) { name.to_string() } else { title.to_string() }
    }

    /// A free name for a page titled `title` in the repo folder `dir`.
    fn claim(&mut self, dir: &Path, title: &str) -> String {
        let dest = self.dest;
        let taken = self.taken.entry(dir.to_path_buf()).or_insert_with(|| {
            let mut names: HashSet<String> = dest
                .read_dir(dir)
                .unwrap_or_default()
                .into_iter()
                .map(|e| {
                    let name = e.name.to_lowercase();
                    name.strip_suffix(".md").map(str::to_string).unwrap_or(name)
                })
                .collect();
            names.insert("index".into());
            names
        });
        let title = match title.trim() {
            "" => "Untitled",
            t => t,
        };
        let mut name = title.to_string();
        let mut n = 2;
        while !taken.insert(name.to_lowercase()) {
            name = format!("{title} {n}");
            n += 1;
        }
        name
    }

    fn write(&self) -> Result<Summary> {
        let mut summary = Summary::default();
        for job in &self.jobs {
            let dest = job.dest();
            let data = match job {
                Job::Page { src, dest } => {
                    summary.pages += 1;
                    let mut text = self.read_text(src)?;
                    if let Some(columns) = self.rows.get(&paths::parent(src)) {
                        text = front_matter(&text, columns);
                    }
                    self.rewrite_links(&text, src, dest).into_bytes()
                }
                Job::Database { csv, title, rows, dest } => {
                    summary.databases += 1;
                    self.database(&self.read_text(csv)?, title, rows.as_deref(), dest)?.into_bytes()
                }
                Job::File { src, .. } => {
                    summary.files += 1;
                    self.src.read(src).with_context(|| format!("reading {}", paths::to_slash(src)))?
                }
            };
            self.dest.write(dest, &data).with_context(|| format!("writing {}", paths::to_slash(dest)))?;
        }
        Ok(summary)
    }

    fn read_text(&self, path: &Path) -> Result<String> {
        let bytes = self.src.read(path).with_context(|| format!("reading {}", paths::to_slash(path)))?;
        let text = String::from_utf8(bytes).with_context(|| format!("{} is not UTF-8", paths::to_slash(path)))?;
        Ok(text.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(text))
    }

    /// Points the links of the page exported at `src` to where their targets are imported.
    /// Scans the text rather than parsing it: Notion writes links with links in their text,
    /// which markdown doesn't count as links. Only paths of exported files are rewritten.
    fn rewrite_links(&self, text: &str, src: &Path, dest: &Path) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(i) = rest.find("](") {
            let (head, tail) = rest.split_at(i + 2);
            out.push_str(head);
            let end = dest_end(tail);
            let link = &tail[..end];
            let found = |d: &str| links::resolve(src, d).and_then(|(t, suffix)| Some((self.map.get(&t)?, suffix)));
            // Notion leaves `#` in paths unescaped, as in `C#/Intro.md`.
            match found(link).or_else(|| found(&link.replace('#', "%23"))) {
                Some((new, suffix)) => {
                    let rel = paths::relative(&paths::parent(dest), new);
                    out.push_str(&format!("{}{suffix}", paths::encode_link(&rel)));
                }
                None => out.push_str(link),
            }
            rest = &tail[end..];
        }
        out.push_str(rest);
        out
    }

    /// The page for a database: its title and its table.
    fn database(&self, csv: &str, title: &str, rows: Option<&Path>, dest: &Path) -> Result<String> {
        let table = self.table(csv, rows, dest)?;
        Ok(if table.is_empty() { format!("# {title}\n") } else { format!("# {title}\n\n{table}") })
    }

    /// A markdown table of the CSV, with each row's title linked to its page in the export
    /// folder `rows`, if any, for a page at `dest`.
    fn table(&self, csv: &str, rows: Option<&Path>, dest: &Path) -> Result<String> {
        let mut out = String::new();
        let table = parse_csv(csv);
        let Some((header, body)) = table.split_first() else { return Ok(out) };

        // Row pages by their (possibly shortened) titles.
        let mut pages: Vec<(String, String)> = Vec::new();
        if let Some(rows) = rows {
            for e in self.read_dir(rows)? {
                let path = rows.join(&e.name);
                if let (Some((t, "md", _)), Some(new)) = (split_id(&e.name), self.map.get(&path)) {
                    pages.push((plain(t), paths::relative(&paths::parent(dest), new)));
                }
            }
        }
        let page_of = |cell: &str| {
            let cell = plain(cell);
            // Notion shortens long titles in file names.
            pages.iter().find(|(t, _)| !t.is_empty() && (*t == cell || t.len() >= 30 && cell.starts_with(t.as_str())))
        };
        let title_col = (0..header.len())
            .max_by_key(|&i| {
                (body.iter().filter(|r| r.get(i).is_some_and(|c| page_of(c).is_some())).count(), Reverse(i))
            })
            .unwrap_or(0);

        let row = |cells: Vec<String>| format!("| {} |\n", cells.join(" | "));
        out.push_str(&row(header.iter().map(|h| cell(h)).collect()));
        out.push_str(&row(header.iter().map(|_| "---".to_string()).collect()));
        for r in body.iter().filter(|r| r.iter().any(|c| !c.is_empty())) {
            let cells = (0..header.len())
                .map(|i| {
                    let value = r.get(i).map(String::as_str).unwrap_or("");
                    match page_of(value).filter(|_| i == title_col) {
                        Some((_, link)) => format!("[{}]({})", cell(value), paths::encode_link(link)),
                        None => cell(value),
                    }
                })
                .collect();
            out.push_str(&row(cells));
        }
        Ok(out)
    }
}

/// Splits `Title <id>.md`, `Title <id>.csv` and `Title <id>_all.csv` into the title, the
/// extension and whether it's the `_all` CSV.
fn split_id(name: &str) -> Option<(&str, &str, bool)> {
    let (stem, ext) = name.rsplit_once('.')?;
    if ext != "md" && ext != "csv" {
        return None;
    }
    let (stem, all) = match stem.strip_suffix("_all") {
        Some(s) if ext == "csv" => (s, true),
        _ => (stem, false),
    };
    let (title, id) = stem.rsplit_once(' ')?;
    (id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())).then_some((title, ext, all))
}

/// Moves the properties Notion writes under a database row's heading, as `Column: value`
/// lines, into YAML front matter.
fn front_matter(text: &str, columns: &[String]) -> String {
    let mut lines = text.split_inclusive('\n');
    let Some(heading) = lines.next().filter(|l| l.starts_with("# ")) else { return text.to_string() };
    let rest: Vec<&str> = lines.collect();
    let blank = |l: &&str| l.trim().is_empty();
    let start = rest.iter().copied().take_while(blank).count();
    let props: Vec<(&str, &str)> = rest[start..]
        .iter()
        .map_while(|l| l.trim_end().split_once(": ").filter(|(k, _)| columns.iter().any(|c| c == k)))
        .collect();
    if props.is_empty() {
        return text.to_string();
    }
    let eol = if heading.ends_with("\r\n") { "\r\n" } else { "\n" };
    let mut out = format!("---{eol}");
    for (k, v) in &props {
        out.push_str(&format!("{}: {}{eol}", yaml(k), yaml(v)));
    }
    out.push_str(&format!("---{eol}{eol}{}", heading.trim_end()));
    out.push_str(eol);
    let body: Vec<&str> = rest[start + props.len()..].iter().copied().skip_while(blank).collect();
    if !body.is_empty() {
        out.push_str(eol);
        out.push_str(&body.concat());
    }
    out
}

/// `s` as a YAML scalar: plain when that reads back the same, double-quoted otherwise.
fn yaml(s: &str) -> String {
    const KEYWORDS: [&str; 8] = ["true", "false", "yes", "no", "on", "off", "null", "~"];
    let plain = !s.is_empty()
        && s.trim() == s
        && !s.starts_with([
            '-', '?', ':', ',', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>', '\'', '"', '%', '@', '`',
        ])
        && !s.contains(": ")
        && !s.contains(" #")
        && !s.ends_with(':')
        && !KEYWORDS.contains(&s.to_lowercase().as_str());
    if plain { s.to_string() } else { format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")) }
}

/// Length of the link destination at the start of `s`: up to the unmatched `)` or a space.
fn dest_end(s: &str) -> usize {
    let mut depth = 0usize;
    for (i, ch) in s.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' if depth == 0 => return i,
            ')' => depth -= 1,
            c if c.is_whitespace() => return i,
            _ => {}
        }
    }
    s.len()
}

/// Letters and digits only, lowercased: titles compared across Notion's file name cleanup.
fn plain(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// A CSV value as a markdown table cell.
fn cell(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").replace('|', "\\|")
}

/// Rows of a CSV file, with quoted fields.
fn parse_csv(s: &str) -> Vec<Vec<String>> {
    let (mut rows, mut row, mut field) = (Vec::new(), Vec::new(), String::new());
    let mut quoted = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => row.push(std::mem::take(&mut field)),
            '\r' if !quoted => {}
            '\n' if !quoted => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            c => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Blob, Files, MemStorage};

    const ID: &str = "0123456789abcdef0123456789abcdef";

    fn mem(files: &[(&str, &str)]) -> MemStorage {
        let files: Files = files
            .iter()
            .map(|(p, c)| (PathBuf::from(p.replace("ID", ID)), Blob::Data(c.replace("ID", ID).into_bytes())))
            .collect();
        MemStorage::new("x", files)
    }

    fn read(fs: &MemStorage, p: &str) -> String {
        fs.read_to_string(Path::new(p)).unwrap_or_else(|e| panic!("{p}: {e}"))
    }

    #[test]
    fn imports_pages_folders_attachments_and_databases() {
        let src = mem(&[
            ("Home ID.md", "# Home\n\n[Work](Home/Work%20ID.md)\n\n[Books](Home/Books%20ID.csv)\n"),
            (
                "Home/Work ID.md",
                "# Work\n\n![](Work/a%20b.png)\n\n[home](../Home%20ID.md#top)\n\n[[C#](x)](C#%20ID.md)\n",
            ),
            ("Home/C# ID.md", "# C#\n"),
            (
                "Home/Item 1 Consider static factory methods instead of  ID.md",
                "# Item 1: Consider static factory methods instead of constructors\n",
            ),
            ("Home/Work/a b.png", "png"),
            ("Home/Books ID.csv", "\u{feff}Name,Note\nDune,\"says \"\"hi\"\"\"\n"),
            ("Home/Books ID_all.csv", "\u{feff}Name,Note\nDune,\"a, b\"\nEmma,\n"),
            ("Home/Books/Dune ID.md", "# Dune\n\nNote: a, b\nName: x: y\n\nGreat book.\n"),
            ("Home/INDEX ID.md", "# INDEX\n"),
            (".DS_Store", ""),
        ]);
        let dest = mem(&[("INDEX.md", "# Notes\n"), ("Home.md", "taken")]);
        let s = notion(&src, &dest, Path::new(""), "assets", Options::default()).unwrap();
        assert_eq!(s, Summary { pages: 6, databases: 1, files: 1 });

        assert_eq!(read(&dest, "Home 2/INDEX.md"), "# Home\n\n[Work](Work.md)\n\n[Books](Books/INDEX.md)\n");
        assert_eq!(
            read(&dest, "Home 2/Work.md"),
            "# Work\n\n![](../assets/Home%202/Work/a%20b.png)\n\n[home](INDEX.md#top)\n\n[[C#](x)](C%23.md)\n"
        );
        assert_eq!(read(&dest, "assets/Home 2/Work/a b.png"), "png");
        assert_eq!(
            read(&dest, "Home 2/Books/Dune.md"),
            "---\nNote: a, b\nName: \"x: y\"\n---\n\n# Dune\n\nGreat book.\n"
        );
        assert_eq!(read(&dest, "Home 2/INDEX 2.md"), "# INDEX\n");
        assert!(dest.exists(Path::new("Home 2/Item 1 Consider static factory methods instead of constructors.md")));
        assert_eq!(
            read(&dest, "Home 2/Books/INDEX.md"),
            "# Books\n\n| Name | Note |\n| --- | --- |\n| [Dune](Dune.md) | a, b |\n| Emma |  |\n"
        );
    }

    #[test]
    fn databases_as_tables() {
        let src = mem(&[
            ("Home ID.md", "# Home\n\n[Books](Home/Books%20ID.csv)\n\nSee [books](Home/Books%20ID.csv).\n"),
            ("Home/Books ID.csv", "Name\nDune\n"),
            ("Home/Books ID_all.csv", "Name,Note\nDune,good\n"),
            ("Home/Books/Dune ID.md", "# Dune\n"),
            ("Home/Films ID.csv", "Name\nAlien\n"),
            ("Home/Films/Alien ID.md", "# Alien\n"),
        ]);
        let dest = mem(&[("INDEX.md", "# Notes\n")]);
        let opts = Options { databases_as_tables: true };
        let s = notion(&src, &dest, Path::new(""), "assets", opts).unwrap();
        assert_eq!(s, Summary { pages: 1, databases: 2, files: 0 });
        assert_eq!(read(&dest, "Home/INDEX.md"), "# Home\n\n[Books](Books.md)\n\nSee [books](Books.md).\n");
        assert_eq!(read(&dest, "Home/Books.md"), "# Books\n\n| Name | Note |\n| --- | --- |\n| Dune | good |\n");
        assert_eq!(read(&dest, "Home/Films.md"), "# Films\n\n| Name |\n| --- |\n| Alien |\n");
        assert!(!dest.exists(Path::new("Home/Books/Dune.md")) && !dest.exists(Path::new("Home/Dune.md")));
    }

    #[test]
    fn front_matter_only_takes_columns() {
        let cols = ["Status".to_string(), "Rating".to_string()];
        assert_eq!(front_matter("# A\n\nStatus: Done", &cols), "---\nStatus: Done\n---\n\n# A\n");
        assert_eq!(front_matter("# A\n\nOther: x\n", &cols), "# A\n\nOther: x\n");
        assert_eq!(
            front_matter("# A\r\n\r\nRating: true\r\nText\r\n", &cols),
            "---\r\nRating: \"true\"\r\n---\r\n\r\n# A\r\n\r\nText\r\n"
        );
        assert_eq!(yaml("\"Hi\" she said"), "\"\\\"Hi\\\" she said\"");
    }

    #[test]
    fn splits_notion_names() {
        assert_eq!(split_id(&format!("My page {ID}.md")), Some(("My page", "md", false)));
        assert_eq!(split_id(&format!("Books {ID}_all.csv")), Some(("Books", "csv", true)));
        assert_eq!(split_id("image.png"), None);
        assert_eq!(split_id("notes 123.md"), None);
    }
}
