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
//! A `>` is also a prefix the engine does not know: the engine splits a
//! word on `:` and on nothing else, so a `>`-line would be searched as the
//! literal text of the line — ">folder" — and would answer nothing. The
//! launcher takes the `>`-lines before the engine ever sees them, and a
//! command the launcher does not know is said out loud rather than being
//! sent on to fail somewhere else.

use crate::actions::Action;
use crate::text::{Lang, Text};

/// The character that starts a command.
pub const COMMAND_PREFIX: &str = ">";

/// One command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    /// The word after the `>`.
    pub name: &'static str,
    /// What it does.
    pub action: Action,
}

/// The commands, in the order the menu lists them.
///
/// The names are the words a person types: `open`, `folder`, `copy`,
/// `preview`, `menu`, `settings`. They are English because they are the
/// keys of an API, and the label the user reads is the action's own
/// label — the two are the same word in the language the user reads.
pub const COMMANDS: &[Command] = &[
    Command {
        name: "open",
        action: Action::Open,
    },
    Command {
        name: "folder",
        action: Action::OpenFolder,
    },
    Command {
        name: "copy",
        action: Action::CopyPath,
    },
    Command {
        name: "preview",
        action: Action::Preview,
    },
    Command {
        name: "menu",
        action: Action::Menu,
    },
    Command {
        name: "settings",
        action: Action::Settings,
    },
];

impl Command {
    /// What the menu shows, in the language the user reads.
    ///
    /// The label is the action's label: a command and the action it
    /// names are the same thing, and two words for one thing is two
    /// words that can drift apart.
    pub fn label(&self, lang: Lang) -> &'static str {
        self.action.label(lang)
    }
}

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
/// which would search for the literal ">" and answer nothing.
pub fn is_command(text: &str) -> bool {
    text.trim_start().starts_with(COMMAND_PREFIX)
}

/// The sentence for a `>`-line that names no command.
///
/// It is said in the language the user reads: the sentence is the
/// vocabulary's, and the vocabulary has one per language.
pub fn unknown(text: &str, lang: Lang) -> alloc::string::String {
    let name = text
        .trim()
        .strip_prefix(COMMAND_PREFIX)
        .unwrap_or("")
        .trim();
    let words = Text::for_lang(lang);
    if name.is_empty() {
        alloc::string::String::from(words.write_a_command)
    } else {
        words.unknown_command.replace("{name}", name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_commands_are_the_actions_the_launcher_has() {
        // Every action the menu offers is a command...
        for action in Action::all() {
            assert!(
                COMMANDS.iter().any(|c| c.action == *action),
                "the menu offers an action that no command names: {action:?}"
            );
        }
        // ...and every command is an action of the launcher, the menu
        // itself included: `>menu` opens the list of the actions.
        for c in COMMANDS {
            assert!(!c.name.is_empty());
            assert!(!c.label(Lang::English).is_empty());
            assert!(!c.label(Lang::Portuguese).is_empty());
            assert!(
                matches!(c.action, Action::Menu | Action::Settings)
                    || Action::all().contains(&c.action),
                "the command >{} is not an action of the launcher",
                c.name
            );
        }
    }

    #[test]
    fn a_command_is_named_after_the_prefix() {
        assert_eq!(
            command_for(">folder").map(|c| c.action),
            Some(Action::OpenFolder)
        );
        assert_eq!(
            command_for(">copy").map(|c| c.action),
            Some(Action::CopyPath)
        );
        assert_eq!(
            command_for("  >open ").map(|c| c.action),
            Some(Action::Open)
        );
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
        // The engine does not know `>`, and it would search for the
        // literal text of the line, so the launcher has to take it before
        // the engine sees it.
        assert!(is_command(">"));
        assert!(is_command(">open"));
        assert!(!is_command("open"));
        assert!(!is_command(""));
    }

    #[test]
    fn the_sentence_names_the_commands_when_none_was_written() {
        let empty = unknown(">", Lang::English);
        assert!(empty.contains(">open"), "{empty}");
        let wrong = unknown(">nope", Lang::English);
        assert!(wrong.contains("nope"), "{wrong}");
    }

    /// The sentence is said in the language the user reads.
    #[test]
    fn the_sentence_is_said_in_the_users_language() {
        let empty = unknown(">", Lang::Portuguese);
        assert!(empty.contains(">open"), "{empty}");
        let wrong = unknown(">nope", Lang::Portuguese);
        assert!(wrong.contains("comando desconhecido"), "{wrong}");
    }

    #[test]
    fn the_sentence_names_every_command_the_table_has() {
        let sentence = unknown(">", Lang::English);
        for command in COMMANDS {
            assert!(
                sentence.contains(&alloc::format!(">{}", command.name)),
                "the sentence does not name >{}: {sentence}",
                command.name
            );
        }
        // And nothing else: a command the table does not have would send
        // the user looking for a word the launcher does not know.
        assert_eq!(
            sentence.matches('>').count(),
            COMMANDS.len(),
            "the sentence names another number of commands: {sentence}"
        );
    }
}
