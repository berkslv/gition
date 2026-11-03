//! Configurable keybindings.

use std::collections::HashMap;

use anyhow::{Result, anyhow, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySpec {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl KeySpec {
    pub fn parse(s: &str) -> Result<KeySpec> {
        let s = s.trim();
        let mut mods = KeyModifiers::NONE;
        let mut rest = s;
        // A bare "+" or a key ending with "++" means the plus key itself.
        loop {
            let lower = rest.to_ascii_lowercase();
            let Some(i) = lower.find('+').filter(|&i| i > 0 && i + 1 < rest.len()) else { break };
            match &lower[..i] {
                "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
                "alt" => mods |= KeyModifiers::ALT,
                "shift" => mods |= KeyModifiers::SHIFT,
                m => bail!("unknown modifier '{m}' in '{s}'"),
            }
            rest = &rest[i + 1..];
        }
        let code = match rest.to_ascii_lowercase().as_str() {
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "space" => KeyCode::Char(' '),
            "backspace" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" => KeyCode::PageDown,
            f if f.len() > 1 && f.starts_with('f') && f[1..].parse::<u8>().is_ok() => {
                KeyCode::F(f[1..].parse().unwrap())
            }
            _ => {
                let mut chars = rest.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => {
                        // Letters with ctrl are matched case-insensitively by terminals.
                        if mods.contains(KeyModifiers::CONTROL) {
                            KeyCode::Char(c.to_ascii_lowercase())
                        } else {
                            KeyCode::Char(c)
                        }
                    }
                    _ => return Err(anyhow!("unknown key '{s}'")),
                }
            }
        };
        Ok(KeySpec { code, mods })
    }

    pub fn matches(&self, ev: &KeyEvent) -> bool {
        let relevant = KeyModifiers::CONTROL | KeyModifiers::ALT;
        match (self.code, ev.code) {
            (KeyCode::Char(a), KeyCode::Char(b)) => {
                let b = if ev.modifiers.contains(KeyModifiers::CONTROL) { b.to_ascii_lowercase() } else { b };
                // Shift is already reflected in the character ('N' vs 'n').
                a == b && (self.mods & relevant) == (ev.modifiers & relevant)
            }
            (a, b) => a == b && self.mods == ev.modifiers,
        }
    }

    pub fn label(&self) -> String {
        let mut s = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            s.push_str("Ctrl+");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            s.push_str("Alt+");
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            s.push_str("Shift+");
        }
        let key = match self.code {
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(c) if !self.mods.is_empty() => c.to_ascii_uppercase().to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "Enter".into(),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Tab => "Tab".into(),
            KeyCode::Up => "↑".into(),
            KeyCode::Down => "↓".into(),
            KeyCode::Left => "←".into(),
            KeyCode::Right => "→".into(),
            KeyCode::PageUp => "PgUp".into(),
            KeyCode::PageDown => "PgDn".into(),
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

    pub fn is(&self, action: Action, ev: &KeyEvent) -> bool {
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

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn parses_and_matches() {
        let k = KeySpec::parse("ctrl+g").unwrap();
        assert!(k.matches(&key(KeyCode::Char('g'), KeyModifiers::CONTROL)));
        assert!(!k.matches(&key(KeyCode::Char('g'), KeyModifiers::NONE)));

        let n = KeySpec::parse("N").unwrap();
        assert!(n.matches(&key(KeyCode::Char('N'), KeyModifiers::SHIFT)));
        assert!(!n.matches(&key(KeyCode::Char('n'), KeyModifiers::NONE)));

        assert_eq!(KeySpec::parse("pgdn").unwrap().code, KeyCode::PageDown);
        assert!(KeySpec::parse("hyper+x").is_err());
    }

    #[test]
    fn config_overrides_defaults() {
        let cfg: Config = toml::from_str("[keys]\nquit = \"ctrl+q\"\nbogus = \"x\"").unwrap();
        let (km, warnings) = Keymap::from_config(&cfg);
        assert!(km.is(Action::Quit, &key(KeyCode::Char('q'), KeyModifiers::CONTROL)));
        assert!(!km.is(Action::Quit, &key(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert_eq!(warnings.len(), 1);
        assert_eq!(km.label(Action::Down), "↓ / j");
    }
}
