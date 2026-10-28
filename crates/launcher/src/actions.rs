// SPDX-License-Identifier: Apache-2.0

//! What can be done with the selected result.
//!
//! The actions are the launcher's, and they are a short list on purpose: a
//! launcher that offers eleven verbs is a launcher the user has to read.
//! Each action names the thing it does and the key that does it, and the
//! platform crate is what carries them out.

/// One action on the selected result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Open it with the platform's default program.
    Open,
    /// Open the folder that holds it.
    OpenFolder,
    /// Copy the path.
    CopyPath,
    /// Show a preview of it.
    Preview,
    /// The list of actions.
    Menu,
}

impl Action {
    /// What the action does, in the words the menu shows.
    pub fn label(&self) -> &'static str {
        match self {
            Action::Open => "Abrir",
            Action::OpenFolder => "Abrir pasta",
            Action::CopyPath => "Copiar caminho",
            Action::Preview => "Pré-visualizar",
            Action::Menu => "Ações",
        }
    }

    /// The key that does it, as the menu shows it.
    pub fn shortcut(&self) -> &'static str {
        match self {
            Action::Open => "Enter",
            Action::OpenFolder => "Ctrl+Enter",
            Action::CopyPath => "Ctrl+C",
            Action::Preview => "Ctrl+P",
            Action::Menu => "Ctrl+K",
        }
    }

    /// The actions the menu lists, in the order it lists them.
    ///
    /// The menu itself is last: it is the action that opens the others, and
    /// a menu that lists itself in the middle reads as a mistake.
    pub fn menu() -> &'static [Action] {
        &[
            Action::Open,
            Action::OpenFolder,
            Action::CopyPath,
            Action::Preview,
            Action::Menu,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_lists_every_action_once() {
        let menu = Action::menu();
        assert_eq!(menu.len(), 5);
        for a in menu {
            assert!(!a.label().is_empty());
            assert!(!a.shortcut().is_empty());
        }
    }

    #[test]
    fn the_menu_action_is_the_last_one() {
        // It is the action that opens the others.
        assert_eq!(Action::menu().last(), Some(&Action::Menu));
    }
}
