use std::collections::{BTreeMap, HashMap};
use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::UiMode;

const MOD_SHIFT: u8 = 0b001;
const MOD_CONTROL: u8 = 0b010;
const MOD_ALT: u8 = 0b100;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KeymapConfig {
    pub modes: Vec<ModeKeymapConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeKeymapConfig {
    pub mode: UiMode,
    pub bindings: Vec<KeyBindingConfig>,
    pub prefixes: Vec<KeyPrefixConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBindingConfig {
    pub keys: Vec<String>,
    pub command_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPrefixConfig {
    pub keys: Vec<String>,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeymapTrie {
    modes: HashMap<UiMode, ModeTrie>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModeTrie {
    root: TrieNode,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct TrieNode {
    command_id: Option<String>,
    prefix_label: Option<String>,
    children: BTreeMap<KeyStroke, TrieNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeymapCompileError {
    DuplicateModeConfig {
        mode: UiMode,
    },
    UnknownCommandId {
        mode: UiMode,
        command_id: String,
    },
    EmptyKeySequence {
        mode: UiMode,
        context: &'static str,
    },
    InvalidKeyToken {
        mode: UiMode,
        token: String,
        message: String,
    },
    DuplicateBinding {
        mode: UiMode,
        sequence: String,
    },
    BindingExtendsExistingCommand {
        mode: UiMode,
        sequence: String,
    },
    BindingShadowsExistingPrefix {
        mode: UiMode,
        sequence: String,
    },
    DuplicatePrefixLabel {
        mode: UiMode,
        sequence: String,
    },
    PrefixConflictsWithCommand {
        mode: UiMode,
        sequence: String,
    },
    PrefixHasNoChildren {
        mode: UiMode,
        sequence: String,
    },
}

impl fmt::Display for KeymapCompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateModeConfig { mode } => {
                write!(
                    f,
                    "duplicate keymap mode configuration for {}",
                    mode.label()
                )
            }
            Self::UnknownCommandId { mode, command_id } => write!(
                f,
                "unknown command id '{command_id}' in {} keymap configuration",
                mode.label()
            ),
            Self::EmptyKeySequence { mode, context } => write!(
                f,
                "empty key sequence in {} keymap {context} entry",
                mode.label()
            ),
            Self::InvalidKeyToken {
                mode,
                token,
                message,
            } => write!(
                f,
                "invalid key token '{token}' in {} keymap: {message}",
                mode.label()
            ),
            Self::DuplicateBinding { mode, sequence } => write!(
                f,
                "duplicate binding for '{sequence}' in {} mode",
                mode.label()
            ),
            Self::BindingExtendsExistingCommand { mode, sequence } => write!(
                f,
                "binding '{sequence}' in {} mode extends an existing command sequence",
                mode.label()
            ),
            Self::BindingShadowsExistingPrefix { mode, sequence } => write!(
                f,
                "binding '{sequence}' in {} mode shadows an existing prefix",
                mode.label()
            ),
            Self::DuplicatePrefixLabel { mode, sequence } => write!(
                f,
                "duplicate prefix label for '{sequence}' in {} mode",
                mode.label()
            ),
            Self::PrefixConflictsWithCommand { mode, sequence } => write!(
                f,
                "prefix '{sequence}' in {} mode conflicts with an existing command sequence",
                mode.label()
            ),
            Self::PrefixHasNoChildren { mode, sequence } => write!(
                f,
                "prefix '{sequence}' in {} mode has no child bindings",
                mode.label()
            ),
        }
    }
}

impl std::error::Error for KeymapCompileError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeymapLookupResult {
    Command {
        command_id: String,
    },
    Prefix {
        label: Option<String>,
        next_keys: Vec<KeyStroke>,
    },
    InvalidPrefix,
    NoMatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyStroke {
    key: KeyCodeToken,
    modifiers: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum KeyCodeToken {
    Char(char),
    Up,
    Down,
    Tab,
    BackTab,
    Enter,
    Backspace,
    Esc,
}

impl fmt::Display for KeyStroke {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if self.modifiers & MOD_CONTROL != 0 {
            parts.push("ctrl".to_owned());
        }
        if self.modifiers & MOD_ALT != 0 {
            parts.push("alt".to_owned());
        }
        if self.modifiers & MOD_SHIFT != 0 {
            parts.push("shift".to_owned());
        }
        let key = match self.key {
            KeyCodeToken::Char(ch) => ch.to_string(),
            KeyCodeToken::Up => "up".to_owned(),
            KeyCodeToken::Down => "down".to_owned(),
            KeyCodeToken::Tab => "tab".to_owned(),
            KeyCodeToken::BackTab => "backtab".to_owned(),
            KeyCodeToken::Enter => "enter".to_owned(),
            KeyCodeToken::Backspace => "backspace".to_owned(),
            KeyCodeToken::Esc => "esc".to_owned(),
        };
        parts.push(key);
        write!(f, "{}", parts.join("+"))
    }
}

impl KeymapTrie {
    pub fn compile<F>(
        config: &KeymapConfig,
        is_known_command: F,
    ) -> Result<Self, KeymapCompileError>
    where
        F: Fn(&str) -> bool,
    {
        let mut modes = HashMap::new();
        for mode_config in &config.modes {
            if modes.contains_key(&mode_config.mode) {
                return Err(KeymapCompileError::DuplicateModeConfig {
                    mode: mode_config.mode,
                });
            }

            let mut trie = ModeTrie::default();

            for binding in &mode_config.bindings {
                if !is_known_command(binding.command_id.as_str()) {
                    return Err(KeymapCompileError::UnknownCommandId {
                        mode: mode_config.mode,
                        command_id: binding.command_id.clone(),
                    });
                }

                let sequence =
                    parse_key_sequence(mode_config.mode, "binding", binding.keys.as_slice())?;
                trie.insert_binding(mode_config.mode, sequence, binding.command_id.clone())?;
            }

            for prefix in &mode_config.prefixes {
                let sequence =
                    parse_key_sequence(mode_config.mode, "prefix", prefix.keys.as_slice())?;
                trie.insert_prefix(mode_config.mode, sequence, prefix.label.clone())?;
            }

            trie.validate_prefixes(mode_config.mode)?;
            modes.insert(mode_config.mode, trie);
        }

        Ok(Self { modes })
    }

    pub fn route_key_event(
        &self,
        mode: UiMode,
        pending: &mut Vec<KeyStroke>,
        key: KeyEvent,
    ) -> KeymapLookupResult {
        let had_pending = !pending.is_empty();
        let Some(stroke) = key_stroke_from_event(key) else {
            pending.clear();
            return if had_pending {
                KeymapLookupResult::InvalidPrefix
            } else {
                KeymapLookupResult::NoMatch
            };
        };

        let mut sequence = pending.clone();
        sequence.push(stroke);

        if let Some(result) = self.resolve_sequence(mode, pending, sequence) {
            return result;
        }

        if had_pending {
            if let Some(result) = self.resolve_sequence(mode, pending, vec![stroke]) {
                return result;
            }
            pending.clear();
            return KeymapLookupResult::InvalidPrefix;
        }

        pending.clear();
        KeymapLookupResult::NoMatch
    }

    pub fn lookup_prefix_label(&self, mode: UiMode, sequence: &[KeyStroke]) -> Option<&str> {
        self.lookup(mode, sequence)
            .and_then(|node| node.prefix_label.as_deref())
    }

    fn lookup(&self, mode: UiMode, sequence: &[KeyStroke]) -> Option<&TrieNode> {
        let trie = self.modes.get(&mode)?;
        let mut node = &trie.root;
        for stroke in sequence {
            node = node.children.get(stroke)?;
        }
        Some(node)
    }

    fn resolve_sequence(
        &self,
        mode: UiMode,
        pending: &mut Vec<KeyStroke>,
        sequence: Vec<KeyStroke>,
    ) -> Option<KeymapLookupResult> {
        let node = self.lookup(mode, sequence.as_slice())?;

        if let Some(command_id) = node.command_id.as_ref() {
            pending.clear();
            return Some(KeymapLookupResult::Command {
                command_id: command_id.clone(),
            });
        }

        if !node.children.is_empty() {
            *pending = sequence;
            return Some(KeymapLookupResult::Prefix {
                label: node.prefix_label.clone(),
                next_keys: node.children.keys().copied().collect(),
            });
        }

        pending.clear();
        Some(KeymapLookupResult::NoMatch)
    }
}

impl Default for ModeTrie {
    fn default() -> Self {
        Self {
            root: TrieNode::default(),
        }
    }
}

impl ModeTrie {
    fn insert_binding(
        &mut self,
        mode: UiMode,
        sequence: Vec<KeyStroke>,
        command_id: String,
    ) -> Result<(), KeymapCompileError> {
        let rendered = format_key_sequence(sequence.as_slice());
        let mut node = &mut self.root;

        for stroke in &sequence[..sequence.len() - 1] {
            node = node.children.entry(*stroke).or_default();
            if node.command_id.is_some() {
                return Err(KeymapCompileError::BindingExtendsExistingCommand {
                    mode,
                    sequence: rendered,
                });
            }
        }

        let leaf = node
            .children
            .entry(sequence[sequence.len() - 1])
            .or_default();
        if leaf.command_id.is_some() {
            return Err(KeymapCompileError::DuplicateBinding {
                mode,
                sequence: rendered,
            });
        }
        if !leaf.children.is_empty() || leaf.prefix_label.is_some() {
            return Err(KeymapCompileError::BindingShadowsExistingPrefix {
                mode,
                sequence: rendered,
            });
        }

        leaf.command_id = Some(command_id);
        Ok(())
    }

    fn insert_prefix(
        &mut self,
        mode: UiMode,
        sequence: Vec<KeyStroke>,
        label: String,
    ) -> Result<(), KeymapCompileError> {
        let rendered = format_key_sequence(sequence.as_slice());
        let mut node = &mut self.root;
        let final_index = sequence.len() - 1;
        for (index, stroke) in sequence.into_iter().enumerate() {
            node = node.children.entry(stroke).or_default();
            if index < final_index && node.command_id.is_some() {
                return Err(KeymapCompileError::PrefixConflictsWithCommand {
                    mode,
                    sequence: rendered,
                });
            }
        }

        if node.command_id.is_some() {
            return Err(KeymapCompileError::PrefixConflictsWithCommand {
                mode,
                sequence: rendered,
            });
        }

        if node.prefix_label.is_some() {
            return Err(KeymapCompileError::DuplicatePrefixLabel {
                mode,
                sequence: rendered,
            });
        }

        node.prefix_label = Some(label);
        Ok(())
    }

    fn validate_prefixes(&self, mode: UiMode) -> Result<(), KeymapCompileError> {
        let mut path = Vec::new();
        validate_prefix_node(mode, &self.root, &mut path)
    }
}

fn validate_prefix_node(
    mode: UiMode,
    node: &TrieNode,
    path: &mut Vec<KeyStroke>,
) -> Result<(), KeymapCompileError> {
    if node.prefix_label.is_some() && node.children.is_empty() {
        return Err(KeymapCompileError::PrefixHasNoChildren {
            mode,
            sequence: format_key_sequence(path.as_slice()),
        });
    }

    for (stroke, child) in &node.children {
        path.push(*stroke);
        validate_prefix_node(mode, child, path)?;
        path.pop();
    }

    Ok(())
}

fn parse_key_sequence(
    mode: UiMode,
    context: &'static str,
    raw_keys: &[String],
) -> Result<Vec<KeyStroke>, KeymapCompileError> {
    if raw_keys.is_empty() {
        return Err(KeymapCompileError::EmptyKeySequence { mode, context });
    }

    raw_keys
        .iter()
        .map(|token| {
            parse_key_token(token.as_str()).map_err(|message| KeymapCompileError::InvalidKeyToken {
                mode,
                token: token.clone(),
                message,
            })
        })
        .collect()
}

fn parse_key_token(raw: &str) -> Result<KeyStroke, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("token is empty".to_owned());
    }

    let parts = trimmed.split('+').collect::<Vec<_>>();
    let mut modifiers = 0u8;
    for modifier in &parts[..parts.len() - 1] {
        let normalized = modifier.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "shift" => modifiers |= MOD_SHIFT,
            "ctrl" | "control" => modifiers |= MOD_CONTROL,
            "alt" => modifiers |= MOD_ALT,
            _ => {
                return Err(format!("unknown modifier '{modifier}'"));
            }
        }
    }

    let key_part = parts[parts.len() - 1].trim();
    if key_part.is_empty() {
        return Err("missing key after modifier".to_owned());
    }

    let key_part_lower = key_part.to_ascii_lowercase();
    let key = match key_part_lower.as_str() {
        "up" => KeyCodeToken::Up,
        "down" => KeyCodeToken::Down,
        "tab" => KeyCodeToken::Tab,
        "backtab" => KeyCodeToken::BackTab,
        "enter" => KeyCodeToken::Enter,
        "backspace" => KeyCodeToken::Backspace,
        "esc" | "escape" => KeyCodeToken::Esc,
        _ => {
            let mut chars = key_part.chars();
            let Some(mut ch) = chars.next() else {
                return Err("missing key token".to_owned());
            };
            if chars.next().is_some() {
                return Err(
                    "keys must be single chars or named keys (tab/backtab/enter/etc.)".to_owned(),
                );
            }
            if ch.is_ascii_uppercase() {
                ch = ch.to_ascii_lowercase();
                modifiers |= MOD_SHIFT;
            }
            KeyCodeToken::Char(ch)
        }
    };

    Ok(KeyStroke { key, modifiers })
}

pub fn key_stroke_from_event(event: KeyEvent) -> Option<KeyStroke> {
    let mut modifiers = normalize_modifiers(event.modifiers);
    let key = match event.code {
        KeyCode::Char(mut ch) => {
            if ch.is_ascii_uppercase() {
                ch = ch.to_ascii_lowercase();
                modifiers |= MOD_SHIFT;
            }
            KeyCodeToken::Char(ch)
        }
        KeyCode::Up => KeyCodeToken::Up,
        KeyCode::Down => KeyCodeToken::Down,
        KeyCode::Tab if modifiers & MOD_SHIFT != 0 => {
            modifiers &= !MOD_SHIFT;
            KeyCodeToken::BackTab
        }
        KeyCode::Tab => KeyCodeToken::Tab,
        KeyCode::BackTab => KeyCodeToken::BackTab,
        KeyCode::Enter => KeyCodeToken::Enter,
        KeyCode::Backspace => KeyCodeToken::Backspace,
        KeyCode::Esc => KeyCodeToken::Esc,
        _ => return None,
    };

    if key == KeyCodeToken::BackTab {
        modifiers &= !(MOD_SHIFT);
    }

    Some(KeyStroke { key, modifiers })
}

fn normalize_modifiers(modifiers: KeyModifiers) -> u8 {
    let mut normalized = 0u8;
    if modifiers.contains(KeyModifiers::SHIFT) {
        normalized |= MOD_SHIFT;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        normalized |= MOD_CONTROL;
    }
    if modifiers.contains(KeyModifiers::ALT) {
        normalized |= MOD_ALT;
    }
    normalized
}

fn format_key_sequence(sequence: &[KeyStroke]) -> String {
    sequence
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_mode(mode: ModeKeymapConfig) -> KeymapConfig {
        KeymapConfig { modes: vec![mode] }
    }

    fn known_command(id: &str) -> bool {
        matches!(id, "ui.focus_next_inbox" | "ui.focus_previous_inbox")
    }

    #[test]
    fn keymap_compiles_bindings_and_prefix_labels() {
        let config = config_with_mode(ModeKeymapConfig {
            mode: UiMode::Normal,
            bindings: vec![
                KeyBindingConfig {
                    keys: vec!["j".to_owned()],
                    command_id: "ui.focus_next_inbox".to_owned(),
                },
                KeyBindingConfig {
                    keys: vec!["z".to_owned(), "k".to_owned()],
                    command_id: "ui.focus_previous_inbox".to_owned(),
                },
            ],
            prefixes: vec![KeyPrefixConfig {
                keys: vec!["z".to_owned()],
                label: "jump".to_owned(),
            }],
        });

        let trie = KeymapTrie::compile(&config, known_command).expect("compiles");

        let prefix = vec![parse_key_token("z").expect("stroke")];
        assert_eq!(
            trie.lookup_prefix_label(UiMode::Normal, &prefix),
            Some("jump")
        );
    }

    #[test]
    fn duplicate_binding_is_rejected() {
        let config = config_with_mode(ModeKeymapConfig {
            mode: UiMode::Normal,
            bindings: vec![
                KeyBindingConfig {
                    keys: vec!["j".to_owned()],
                    command_id: "ui.focus_next_inbox".to_owned(),
                },
                KeyBindingConfig {
                    keys: vec!["j".to_owned()],
                    command_id: "ui.focus_previous_inbox".to_owned(),
                },
            ],
            prefixes: vec![],
        });

        let err = KeymapTrie::compile(&config, known_command).expect_err("duplicate");
        assert!(matches!(err, KeymapCompileError::DuplicateBinding { .. }));
    }

    #[test]
    fn command_prefix_conflicts_are_rejected() {
        let config = config_with_mode(ModeKeymapConfig {
            mode: UiMode::Normal,
            bindings: vec![
                KeyBindingConfig {
                    keys: vec!["g".to_owned()],
                    command_id: "ui.focus_next_inbox".to_owned(),
                },
                KeyBindingConfig {
                    keys: vec!["g".to_owned(), "k".to_owned()],
                    command_id: "ui.focus_previous_inbox".to_owned(),
                },
            ],
            prefixes: vec![],
        });

        let err = KeymapTrie::compile(&config, known_command).expect_err("prefix conflict");
        assert!(matches!(
            err,
            KeymapCompileError::BindingExtendsExistingCommand { .. }
        ));
    }

    #[test]
    fn prefix_extending_command_is_rejected() {
        let config = config_with_mode(ModeKeymapConfig {
            mode: UiMode::Normal,
            bindings: vec![KeyBindingConfig {
                keys: vec!["g".to_owned()],
                command_id: "ui.focus_next_inbox".to_owned(),
            }],
            prefixes: vec![KeyPrefixConfig {
                keys: vec!["g".to_owned(), "k".to_owned()],
                label: "invalid".to_owned(),
            }],
        });

        let err = KeymapTrie::compile(&config, known_command).expect_err("prefix conflict");
        assert!(matches!(
            err,
            KeymapCompileError::PrefixConflictsWithCommand { .. }
        ));
    }

    #[test]
    fn unknown_command_ids_are_rejected() {
        let config = config_with_mode(ModeKeymapConfig {
            mode: UiMode::Normal,
            bindings: vec![KeyBindingConfig {
                keys: vec!["j".to_owned()],
                command_id: "ui.unknown".to_owned(),
            }],
            prefixes: vec![],
        });

        let err = KeymapTrie::compile(&config, known_command).expect_err("unknown id");
        assert!(matches!(err, KeymapCompileError::UnknownCommandId { .. }));
    }

    #[test]
    fn runtime_lookup_reports_prefix_and_invalid_prefix() {
        let config = config_with_mode(ModeKeymapConfig {
            mode: UiMode::Normal,
            bindings: vec![KeyBindingConfig {
                keys: vec!["z".to_owned(), "k".to_owned()],
                command_id: "ui.focus_previous_inbox".to_owned(),
            }],
            prefixes: vec![KeyPrefixConfig {
                keys: vec!["z".to_owned()],
                label: "jump".to_owned(),
            }],
        });

        let trie = KeymapTrie::compile(&config, known_command).expect("compiles");
        let mut pending = Vec::new();

        let first = trie.route_key_event(
            UiMode::Normal,
            &mut pending,
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
        );
        assert!(matches!(
            first,
            KeymapLookupResult::Prefix {
                label: Some(ref label),
                ..
            } if label == "jump"
        ));
        assert_eq!(pending.len(), 1);

        let invalid = trie.route_key_event(
            UiMode::Normal,
            &mut pending,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert!(matches!(invalid, KeymapLookupResult::InvalidPrefix));
        assert!(pending.is_empty());
    }

    #[test]
    fn invalid_prefix_falls_back_to_standalone_command() {
        let config = config_with_mode(ModeKeymapConfig {
            mode: UiMode::Normal,
            bindings: vec![
                KeyBindingConfig {
                    keys: vec!["q".to_owned()],
                    command_id: "ui.focus_next_inbox".to_owned(),
                },
                KeyBindingConfig {
                    keys: vec!["z".to_owned(), "k".to_owned()],
                    command_id: "ui.focus_previous_inbox".to_owned(),
                },
            ],
            prefixes: vec![KeyPrefixConfig {
                keys: vec!["z".to_owned()],
                label: "jump".to_owned(),
            }],
        });

        let trie = KeymapTrie::compile(&config, known_command).expect("compiles");
        let mut pending = Vec::new();

        let first = trie.route_key_event(
            UiMode::Normal,
            &mut pending,
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
        );
        assert!(matches!(first, KeymapLookupResult::Prefix { .. }));
        assert_eq!(pending.len(), 1);

        let fallback = trie.route_key_event(
            UiMode::Normal,
            &mut pending,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );
        assert!(matches!(
            fallback,
            KeymapLookupResult::Command { ref command_id } if command_id == "ui.focus_next_inbox"
        ));
        assert!(pending.is_empty());
    }

    #[test]
    fn shift_tab_event_normalizes_to_backtab() {
        let from_shift_tab =
            key_stroke_from_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT))
                .expect("stroke");
        let from_backtab = parse_key_token("backtab").expect("backtab token");
        assert_eq!(from_shift_tab, from_backtab);
    }
}
