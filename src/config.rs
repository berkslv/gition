//! Per-repo configuration, read from `.gition.toml` at the repo root.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use ratatui::style::{Color, Style};
use serde::{Deserialize, Deserializer};

pub const FILE_NAME: &str = ".gition.toml";

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub tree_width: u16,
    pub assets_dir: String,
    pub show_all_files: bool,
    pub commit_prefix: String,
    /// Pasting a bare URL in the editor turns it into a link titled after its page.
    pub link_titles: bool,
    /// Pasted and inserted PNG/JPEG images are saved as lossy WebP when that's smaller.
    pub compress_images: bool,
    /// Action name -> one key or a list of keys, e.g. `quit = ["q", "ctrl+q"]`.
    pub keys: BTreeMap<String, KeyList>,
    pub theme: Theme,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum KeyList {
    One(String),
    Many(Vec<String>),
}

impl KeyList {
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            KeyList::One(s) => vec![s.clone()],
            KeyList::Many(v) => v.clone(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tree_width: 30,
            assets_dir: "assets".into(),
            show_all_files: false,
            commit_prefix: "gition:".into(),
            link_titles: true,
            compress_images: true,
            keys: BTreeMap::new(),
            theme: Theme::default(),
        }
    }
}

macro_rules! theme {
    ($($(#[doc = $doc:literal])* $name:ident,)*) => {
        /// Colors by role: a `preset`, then any of its colors overridden one by one.
        #[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
        #[serde(try_from = "ThemeFile")]
        pub struct Theme {
            $($(#[doc = $doc])* pub $name: Color,)*
        }

        /// `[theme]` as written. Each color is a name (`cyan`, `dark_gray`), `#rrggbb`, or a
        /// 256-color index.
        #[derive(Default, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct ThemeFile {
            preset: Option<String>,
            $(#[serde(deserialize_with = "color")] $name: Option<Color>,)*
        }

        impl TryFrom<ThemeFile> for Theme {
            type Error = String;

            fn try_from(f: ThemeFile) -> Result<Self, String> {
                let name = f.preset.as_deref().unwrap_or("default");
                let base = Theme::preset(name)
                    .ok_or_else(|| format!("unknown theme preset {name:?}; use one of {}", PRESETS.join(", ")))?;
                Ok(Theme { $($name: f.$name.unwrap_or(base.$name),)* })
            }
        }
    };
}

theme! {
    /// Focused borders, prompts and help headings.
    accent,
    /// Hints, unfocused borders, quotes and front matter.
    dim,
    /// Background of the cursor row.
    cursor,
    /// Background of the status bar.
    bar,
    /// Background of selected text.
    selection,
    folder,
    /// The open page in the tree.
    current,
    ok,
    warning,
    error,
    heading1,
    heading2,
    heading3,
    link,
    code,
    /// List bullets and task boxes.
    bullet,
    /// Image placeholders.
    image,
}

pub const PRESETS: [&str; 6] = ["default", "light", "nord", "gruvbox", "dracula", "catppuccin"];

impl Default for Theme {
    fn default() -> Self {
        Self::preset("default").unwrap()
    }
}

impl Theme {
    /// A built-in theme. `default` and `light` use the terminal's own 16 colors; the others
    /// need a terminal with true color.
    pub fn preset(name: &str) -> Option<Theme> {
        use Color::*;
        let hex = |c: u32| Rgb((c >> 16) as u8, (c >> 8) as u8, c as u8);
        Some(match name {
            "default" => Theme {
                accent: Cyan,
                dim: DarkGray,
                cursor: Indexed(236),
                bar: Indexed(235),
                selection: Indexed(24),
                folder: Blue,
                current: Green,
                ok: Green,
                warning: Yellow,
                error: Red,
                heading1: Magenta,
                heading2: Cyan,
                heading3: Blue,
                link: Blue,
                code: Yellow,
                bullet: Cyan,
                image: Magenta,
            },
            "light" => Theme {
                accent: Blue,
                dim: DarkGray,
                cursor: Indexed(254),
                bar: Indexed(253),
                selection: Indexed(25),
                folder: Blue,
                current: Green,
                ok: Green,
                warning: Indexed(130),
                error: Red,
                heading1: Magenta,
                heading2: Blue,
                heading3: Cyan,
                link: Blue,
                code: Indexed(130),
                bullet: Blue,
                image: Magenta,
            },
            "nord" => Theme {
                accent: hex(0x88c0d0),
                dim: hex(0x616e88),
                cursor: hex(0x434c5e),
                bar: hex(0x3b4252),
                selection: hex(0x5e81ac),
                folder: hex(0x81a1c1),
                current: hex(0xa3be8c),
                ok: hex(0xa3be8c),
                warning: hex(0xebcb8b),
                error: hex(0xbf616a),
                heading1: hex(0xb48ead),
                heading2: hex(0x88c0d0),
                heading3: hex(0x81a1c1),
                link: hex(0x8fbcbb),
                code: hex(0xd08770),
                bullet: hex(0x88c0d0),
                image: hex(0xb48ead),
            },
            "gruvbox" => Theme {
                accent: hex(0xfe8019),
                dim: hex(0x928374),
                cursor: hex(0x3c3836),
                bar: hex(0x32302f),
                selection: hex(0x665c54),
                folder: hex(0x83a598),
                current: hex(0xb8bb26),
                ok: hex(0xb8bb26),
                warning: hex(0xfabd2f),
                error: hex(0xfb4934),
                heading1: hex(0xfe8019),
                heading2: hex(0xfabd2f),
                heading3: hex(0x8ec07c),
                link: hex(0x83a598),
                code: hex(0xd3869b),
                bullet: hex(0xfe8019),
                image: hex(0xd3869b),
            },
            "dracula" => Theme {
                accent: hex(0xbd93f9),
                dim: hex(0x6272a4),
                cursor: hex(0x44475a),
                bar: hex(0x21222c),
                selection: hex(0x6272a4),
                folder: hex(0x8be9fd),
                current: hex(0x50fa7b),
                ok: hex(0x50fa7b),
                warning: hex(0xffb86c),
                error: hex(0xff5555),
                heading1: hex(0xff79c6),
                heading2: hex(0xbd93f9),
                heading3: hex(0x8be9fd),
                link: hex(0x8be9fd),
                code: hex(0xf1fa8c),
                bullet: hex(0xff79c6),
                image: hex(0xbd93f9),
            },
            "catppuccin" => Theme {
                accent: hex(0xcba6f7),
                dim: hex(0x7f849c),
                cursor: hex(0x313244),
                bar: hex(0x181825),
                selection: hex(0x45475a),
                folder: hex(0x89b4fa),
                current: hex(0xa6e3a1),
                ok: hex(0xa6e3a1),
                warning: hex(0xf9e2af),
                error: hex(0xf38ba8),
                heading1: hex(0xf5c2e7),
                heading2: hex(0xcba6f7),
                heading3: hex(0x89b4fa),
                link: hex(0x89dceb),
                code: hex(0xfab387),
                bullet: hex(0x94e2d5),
                image: hex(0xf5c2e7),
            },
            _ => return None,
        })
    }

    /// How selected text looks, in the viewer and the editor.
    pub fn selected(&self) -> Style {
        Style::new().bg(self.selection).fg(Color::White)
    }
}

fn color<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Color>, D::Error> {
    let s = String::deserialize(d)?;
    s.parse().map(Some).map_err(|_| serde::de::Error::custom(format!("unknown color {s:?}")))
}

impl Config {
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(FILE_NAME);
        if !path.exists() {
            return Ok(Self::default());
        }
        Self::parse(&std::fs::read_to_string(&path)?)
    }

    pub fn parse(text: &str) -> Result<Self> {
        let cfg: Self = toml::from_str(text).with_context(|| format!("invalid {FILE_NAME}"))?;
        // A cloned repo's config must not send pasted images outside the repo.
        if crate::paths::normalize(Path::new(&cfg.assets_dir)).is_none() {
            anyhow::bail!("invalid {FILE_NAME}: assets_dir must be a path inside the repo");
        }
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_theme_colors() {
        let cfg = Config::parse("[theme]\naccent = \"#ff8800\"\ndim = \"dark_gray\"\ncursor = \"237\"\n").unwrap();
        assert_eq!(cfg.theme.accent, Color::Rgb(255, 136, 0));
        assert_eq!(cfg.theme.dim, Color::DarkGray);
        assert_eq!(cfg.theme.cursor, Color::Indexed(237));
        assert_eq!(cfg.theme.link, Color::Blue);
        assert!(Config::parse("[theme]\naccent = \"nope\"\n").is_err());
        assert!(Config::parse("[theme]\nfoo = \"red\"\n").is_err());

        let cfg = Config::parse("[theme]\npreset = \"nord\"\nlink = \"red\"\n").unwrap();
        assert_eq!(cfg.theme.accent, Theme::preset("nord").unwrap().accent);
        assert_eq!(cfg.theme.link, Color::Red);
        assert!(Config::parse("[theme]\npreset = \"nope\"\n").is_err());
        assert!(PRESETS.iter().all(|p| Theme::preset(p).is_some()));
    }

    #[test]
    fn assets_dir_stays_in_the_repo() {
        assert!(Config::parse("assets_dir = \"media/img\"\n").is_ok());
        for bad in ["/tmp/x", "../outside", "a/../../b"] {
            assert!(Config::parse(&format!("assets_dir = \"{bad}\"\n")).is_err(), "{bad}");
        }
    }

    #[test]
    fn starter_config_is_the_default() {
        let (_, text) = crate::repo::starter_files("x", "assets").into_iter().find(|(p, _)| p == FILE_NAME).unwrap();
        let cfg = Config::parse(&text).unwrap();
        assert_eq!(cfg.theme, Theme::default());
        assert_eq!(cfg.tree_width, Config::default().tree_width);

        // The commented-out colors are the default preset's, under their real names.
        let theme = &text[text.find("[theme]").unwrap()..];
        let uncommented: String = theme
            .lines()
            .map(|l| l.strip_prefix("# ").filter(|l| l.contains(" = ")).unwrap_or(l))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(Config::parse(&uncommented).unwrap().theme, Theme::default());
    }
}
