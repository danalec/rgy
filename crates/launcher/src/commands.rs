// SPDX-License-Identifier: Apache-2.0

//! The quick commands.
//!
//! Some things a launcher does are not searches: opening the folder that
//! holds the result, copying the path, showing what is inside the file.
//! Those are the actions of the selected result, and they are also things
//! a user reaches for without thinking about the result at all.
//!
//! The **command bar** is where the two meet. A line that starts with `>`
//! is a command, not a query: `>folder` opens the folder, `>copy` puts the
//! path on the clipboard. The prefix is what keeps the two apart — a
//! launcher where typing `copy` searched for files called "copy" would be
//! a launcher that ran the wrong thing half the time.
//!
//! A `>` is also a prefix the engine does not know, and the engine fails
//! closed on one: so the launcher takes the `>`-lines before the engine
//! ever sees them. A command the launcher does not know is said out loud
//! rather than being sent on to fail somewhere else.

use crate::actions::Action;

/// The character that starts a command.
pub const COMMAND_PREFIX: &str = ">";

/// One command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    /// The word after the `>`.
    pub name: &'static str,
    /// What it does.
    pub action: Action,
    /// What the menu shows.
    pub label: &'static str,
}

/// The commands, in the order the menu lists them.
///
/// The names are the words a person types: `open`, `folder`, `copy`,
/// `preview`, `menu`. They are English because they are the keys of an
/// API — the labels the user reads are Portuguese, and the two are not
/// required to be the same language.
pub const COMMANDS: &[Command] = &[
    Command {
        name: "open",
        action: Action::Open,
        label: "Abrir",
    },
    Command {
        name: "folder",
        action: Action::OpenFolder,
        label: "Abrir pasta",
    },
    Command {
        name: "copy",
        action: Action::CopyPath,
        label: "Copiar caminho",
    },
    Command {
        name: "preview",
        action: Action::Preview,
        label: "Pré-visualizar",
    },
    Command {
        name: "menu",
        action: Action::Menu,
        label: "Ações",
    },
];

/// The command a text names, if it names one.
///
/// The text has to start with the prefix and name a command: `>folder`
/// is the command, `>folders` is not, and `folder` is a search.
pub fn command_for(text: &str) -> Option<&'static Command> {
    let name = text.trim().strip_prefix(COMMAND_PREFIX)?.trim();
    COMMANDS.iter().find(|c| c.name == name)
}

/// Whether the text is a command line at all.
///
/// A `>` with nothing after it is the prefix being typed, and it is
/// still a command line: the launcher must not send it to the engine,
/// which would fail it closed.
pub fn is_command(text: &str) -> bool {
    text.trim_start().starts_with(COMMAND_PREFIX)
}

/// The sentence for a `>`-line that names no command.
pub fn unknown(text: &str) -> alloc::string::String {
    let name = text.trim().strip_prefix(COMMAND_PREFIX).unwrap_or("").trim();
    if name.is_empty() {
        alloc::string::String::from("escreva um comando: >open, >folder, >copy, >preview")
    } else {
        alloc::format!("comando desconhecido: >{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_commands_are_the_actions_the_launcher_has() {
        assert_eq!(COMMANDS.len(), Action::menu().len());
        for c in COMMANDS {
            assert!(!c.name.is_empty());
            assert!(!c.label.is_empty());
            assert!(Action::menu().contains(&c.action));
        }
    }

    #[test]
    fn a_command_is_named_after_the_prefix() {
        assert_eq!(command_for(">folder").map(|c| c.action), Some(Action::OpenFolder));
        assert_eq!(command_for(">copy").map(|c| c.action), Some(Action::CopyPath));
        assert_eq!(command_for("  >open ").map(|c| c.action), Some(Action::Open));
    }

    #[test]
    fn a_word_without_the_prefix_is_a_search_and_not_a_command() {
        assert!(command_for("folder").is_none());
        assert!(command_for("copy").is_none());
    }

    #[test]
    fn a_command_that_does_not_exist_is_not_a_command() {
        assert!(command_for(">folders").is_none());
        assert!(command_for(">").is_none());
    }

    #[test]
    fn a_prefix_alone_is_a_command_line() {
        // The engine does not know `>` and fails closed on it, so the
        // launcher has to take it before the engine sees it.
        assert!(is_command(">"));
        assert!(is_command(">open"));
        assert!(!is_command("open"));
        assert!(!is_command(""));
    }

    #[test]
    fn the_sentence_names_the_commands_when_none_was_written() {
        let empty = unknown(">");
        assert!(empty.contains(">open"), "{empty}");
        let wrong = unknown(">nope");
        assert!(wrong.contains("nope"), "{wrong}");
    }
}
