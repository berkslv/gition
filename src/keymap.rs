//! Configurable keybindings.

use std::collections::HashMap;

use anyhow::{Result, anyhow, bail};
use ratatui_textarea::{Input, Key};

use crate::config::Config;

macro_rules! actions {
    ($($variant:ident => $name:literal, $desc:literal, [$($key:literal),*];)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Action { $($variant),* }

        impl Action {
            pub const ALL: &[Action] = &[$(Action::$variant),*];
            pub fn name(self) -> &'static str { match self { $(Action::$variant => $name),* } }
            pub fn description(self) -> &'static str { match self { $(Action::$variant => $desc),* } }
            fn default_keys(self) -> &'static [&'static str] { match self { $(Action::$variant => &[$($key),*]),* } }
            pub fn from_name(s: &str) -> Option<Action> { match s { $($name => Some(Action::$variant),)* _ => None } }
        }
    };
}

actions! {
    // global
    Quit => "quit", "quit", ["q"];
    Help => "help", "show this help", ["?"];
    Search => "search", "fuzzy-find a page", ["/"];
    Sync => "sync", "save, commit everything, pull --rebase, push", ["ctrl+g"];
    ToggleTree => "toggle_tree", "show/hide the tree", ["ctrl+b"];
    SwitchFocus => "switch_focus", "switch focus tree <-> page", ["tab"];
    // tree
    Up => "up", "move up", ["up", "k"];
    Down => "down", "move down", ["down", "j"];
    Open => "open", "open page / expand folder", ["enter", "right", "l"];
    Collapse => "collapse", "collapse / go to parent", ["left", "h"];
    NewPage => "new_page", "new page", ["n"];
    NewFolder => "new_folder", "new folder page", ["N"];
    Rename => "rename", "rename or move (type a path)", ["r"];
    Delete => "delete", "delete", ["d"];
    InsertImage => "insert_image", "insert image into current page", ["i"];
    ToggleAllFiles => "toggle_all_files", "show all files", ["."];
    // view
    PageUp => "page_up", "page up", ["pageup"];
    PageDown => "page_down", "page down", ["pagedown"];
    Edit => "edit", "edit page", ["e"];
    Follow => "follow", "follow link / open image on line", ["enter"];
    OpenExternal => "open_external", "open with system app", ["o"];
    // edit
    Save => "save", "save", ["ctrl+s"];
    Undo => "undo", "undo", ["ctrl+z"];
    Redo => "redo", "redo", ["ctrl+y"];
    ExitEdit => "exit_edit", "back to view", ["esc"];
}

/// A key with modifiers. Matched against [`Input`], the backend-agnostic key type
/// shared by the terminal and web frontends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySpec {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl KeySpec {
    pub fn parse(s: &str) -> Result<KeySpec> {
        let s = s.trim();
        let (mut ctrl, mut alt, mut shift) = (false, false, false);
        let mut rest = s;
        // A bare "+" or a key ending with "++" means the plus key itself.
        loop {
            let lower = rest.to_ascii_lowercase();
            let Some(i) = lower.find('+').filter(|&i| i > 0 && i + 1 < rest.len()) else { break };
            match &lower[..i] {
                "ctrl" | "control" => ctrl = true,
                "alt" => alt = true,
                "shift" => shift = true,
                m => bail!("unknown modifier '{m}' in '{s}'"),
            }
            rest = &rest[i + 1..];
        }
        let key = match rest.to_ascii_lowercase().as_str() {
            "enter" | "return" => Key::Enter,
            "esc" | "escape" => Key::Esc,
            "tab" => Key::Tab,
            "backtab" => {
                shift = true;
                Key::Tab
            }
            "space" => Key::Char(' '),
            "backspace" => Key::Backspace,
            "delete" | "del" => Key::Delete,
            "up" => Key::Up,
            "down" => Key::Down,
            "left" => Key::Left,
            "right" => Key::Right,
            "home" => Key::Home,
            "end" => Key::End,
            "pageup" | "pgup" => Key::PageUp,
            "pagedown" | "pgdn" => Key::PageDown,
            f if f.len() > 1 && f.starts_with('f') && f[1..].parse::<u8>().is_ok() => Key::F(f[1..].parse().unwrap()),
            _ => {
                let mut chars = rest.chars();
                match (chars.next(), chars.next()) {
                    // Letters with ctrl are matched case-insensitively by terminals.
                    (Some(c), None) if ctrl => Key::Char(c.to_ascii_lowercase()),
                    (Some(c), None) => Key::Char(c),
                    _ => return Err(anyhow!("unknown key '{s}'")),
                }
            }
        };
        Ok(KeySpec { key, ctrl, alt, shift })
    }

    pub fn matches(&self, ev: &Input) -> bool {
        match (self.key, ev.key) {
            (Key::Char(a), Key::Char(b)) => {
                let b = if ev.ctrl { b.to_ascii_lowercase() } else { b };
                // Shift is already reflected in the character ('N' vs 'n').
                a == b && self.ctrl == ev.ctrl && self.alt == ev.alt
            }
            (a, b) => a == b && self.ctrl == ev.ctrl && self.alt == ev.alt && self.shift == ev.shift,
        }
    }

    pub fn label(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("Ctrl+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        if self.shift {
            s.push_str("Shift+");
        }
        let key = match self.key {
            Key::Char(' ') => "Space".to_string(),
            Key::Char(c) if self.ctrl || self.alt || self.shift => c.to_ascii_uppercase().to_string(),
            Key::Char(c) => c.to_string(),
            Key::Enter => "Enter".into(),
            Key::Esc => "Esc".into(),
            Key::Tab => "Tab".into(),
            Key::Up => "↑".into(),
            Key::Down => "↓".into(),
            Key::Left => "←".into(),
            Key::Right => "→".into(),
            Key::PageUp => "PgUp".into(),
            Key::PageDown => "PgDn".into(),
            Key::F(n) => format!("F{n}"),
            other => format!("{other:?}"),
        };
        s.push_str(&key);
        s
    }
}

#[derive(Debug, Clone)]
pub struct Keymap {
    map: HashMap<Action, Vec<KeySpec>>,
}

impl Keymap {
    /// Builds the keymap from defaults plus overrides in the config.
    /// Invalid entries are skipped and reported as warnings.
    pub fn from_config(cfg: &Config) -> (Keymap, Vec<String>) {
        let mut map = HashMap::new();
        for &a in Action::ALL {
            let keys = a.default_keys().iter().map(|k| KeySpec::parse(k).expect("valid default key")).collect();
            map.insert(a, keys);
        }
        let mut warnings = Vec::new();
        for (name, keys) in &cfg.keys {
            let Some(action) = Action::from_name(name) else {
                warnings.push(format!("unknown action '{name}' in config"));
                continue;
            };
            let mut specs = Vec::new();
            for k in keys.to_vec() {
                match KeySpec::parse(&k) {
                    Ok(spec) => specs.push(spec),
                    Err(e) => warnings.push(e.to_string()),
                }
            }
            map.insert(action, specs);
        }
        (Keymap { map }, warnings)
    }

    pub fn is(&self, action: Action, ev: &Input) -> bool {
        self.map.get(&action).is_some_and(|keys| keys.iter().any(|k| k.matches(ev)))
    }

    pub fn label(&self, action: Action) -> String {
        self.map
            .get(&action)
            .map(|keys| keys.iter().map(KeySpec::label).collect::<Vec<_>>().join(" / "))
            .unwrap_or_default()
    }
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap::from_config(&Config::default()).0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Key, ctrl: bool, shift: bool) -> Input {
        Input { key, ctrl, alt: false, shift }
    }

    #[test]
    fn parses_and_matches() {
        let k = KeySpec::parse("ctrl+g").unwrap();
        assert!(k.matches(&key(Key::Char('g'), true, false)));
        assert!(!k.matches(&key(Key::Char('g'), false, false)));

        let n = KeySpec::parse("N").unwrap();
        assert!(n.matches(&key(Key::Char('N'), false, true)));
        assert!(!n.matches(&key(Key::Char('n'), false, false)));

        assert_eq!(KeySpec::parse("pgdn").unwrap().key, Key::PageDown);
        assert!(KeySpec::parse("hyper+x").is_err());
    }

    #[test]
    fn config_overrides_defaults() {
        let cfg: Config = toml::from_str("[keys]\nquit = \"ctrl+q\"\nbogus = \"x\"").unwrap();
        let (km, warnings) = Keymap::from_config(&cfg);
        assert!(km.is(Action::Quit, &key(Key::Char('q'), true, false)));
        assert!(!km.is(Action::Quit, &key(Key::Char('q'), false, false)));
        assert_eq!(warnings.len(), 1);
        assert_eq!(km.label(Action::Down), "↓ / j");
    }
}
