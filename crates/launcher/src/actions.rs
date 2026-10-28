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
    ///
    /// It is the action that opens the others, and it is not one of the
    /// ones the list shows: `Ctrl+K` and `>menu` still open it.
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
    /// The menu is not in the list: it is the action that opens the
    /// others, and a menu that lists itself in the middle reads as a
    /// mistake. Picking "Ações" in the menu would open the menu the user
    /// is already in.
    pub fn menu() -> &'static [Action] {
        &[
            Action::Open,
            Action::OpenFolder,
            Action::CopyPath,
            Action::Preview,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_lists_the_actions_of_the_result_once_each() {
        let menu = Action::menu();
        assert_eq!(menu.len(), 4);
        for a in menu {
            assert!(!a.label().is_empty());
            assert!(!a.shortcut().is_empty());
        }
    }

    #[test]
    fn the_menu_does_not_offer_the_menu() {
        // It is the action that opens the others: offering it in the
        // list would open the menu the user is already in.
        assert!(!Action::menu().contains(&Action::Menu));
    }
}
