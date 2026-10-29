// SPDX-License-Identifier: Apache-2.0

//! What can be done with the selected result.
//!
//! The actions are the launcher's, and they are a short list on purpose: a
//! launcher that offers eleven verbs is a launcher the user has to read.
//! Each action names the thing it does and the key that does it, and the
//! platform crate is what carries them out.
//!
//! The list **follows the row**: an action that cannot work for the result
//! that is selected is not offered. The preview reads the head of a file as
//! text, and for a folder, an image, a video, a sound or a program that
//! head is noise — a menu that offers the preview and then fails is a menu
//! that lies.

use alloc::string::String;

use crate::results::IconKind;

/// The actions every result has.
///
/// Opening the result, opening the folder that holds it and copying its
/// path work on anything the machine can open.
const COMMON: &[Action] = &[Action::Open, Action::OpenFolder, Action::CopyPath];

/// The actions of a result whose bytes can be read as text.
///
/// The preview is the head of the file as text, and it is worth offering
/// only where that head says something.
const WITH_PREVIEW: &[Action] = &[
    Action::Open,
    Action::OpenFolder,
    Action::CopyPath,
    Action::Preview,
];

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

    /// Every action the launcher has, in the order the menu lists them.
    ///
    /// The menu is not in the list: it is the action that opens the
    /// others, and a menu that lists itself in the middle reads as a
    /// mistake. Picking "Ações" in the menu would open the menu the user
    /// is already in.
    ///
    /// This is the **whole** set: what a result is offered is
    /// [`Action::for_kind`], and a result that cannot be previewed does
    /// not get the preview in its list.
    pub fn all() -> &'static [Action] {
        WITH_PREVIEW
    }

    /// The actions offered for a kind of thing.
    ///
    /// The preview needs a file whose bytes can be read as text: a
    /// document and a file of no kind of its own. The other three are
    /// offered for every row — a folder is opened, the folder that holds
    /// it is opened and its path is copied like anybody else's.
    pub fn for_kind(kind: IconKind) -> &'static [Action] {
        match kind {
            IconKind::Document | IconKind::File => WITH_PREVIEW,
            _ => COMMON,
        }
    }

    /// The sentence the launcher says when the action does not fit.
    ///
    /// The refusal says **why**, in the user's language: a key that was
    /// pressed and refused in silence is a key the user presses again.
    ///
    /// `None` when the action fits the row — the launcher does not
    /// refuse what it offers.
    pub fn refusal(&self, kind: IconKind) -> Option<String> {
        match self {
            Action::Preview if !Self::for_kind(kind).contains(&Action::Preview) => Some(
                alloc::format!("{} não tem pré-visualização", thing_of(kind)),
            ),
            _ => None,
        }
    }
}

/// The name of a kind of thing, in the user's language.
///
/// It is what the refusal names: the user does not have to guess which
/// thing the launcher refused.
fn thing_of(kind: IconKind) -> &'static str {
    match kind {
        IconKind::Folder => "uma pasta",
        IconKind::Document => "um documento",
        IconKind::Image => "uma imagem",
        IconKind::Video => "um vídeo",
        IconKind::Audio => "um áudio",
        IconKind::Application => "um programa",
        IconKind::File => "um ficheiro",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_of_the_launcher_is_listed_once_each() {
        let all = Action::all();
        assert_eq!(all.len(), 4);
        for a in all {
            assert!(!a.label().is_empty());
            assert!(!a.shortcut().is_empty());
        }
    }

    #[test]
    fn the_menu_does_not_offer_the_menu() {
        // It is the action that opens the others: offering it in the
        // list would open the menu the user is already in.
        assert!(!Action::all().contains(&Action::Menu));
    }

    /// A folder cannot be previewed.
    ///
    /// The preview is the head of a file as text, and a folder has no
    /// head: the action would fail on every folder of the index.
    #[test]
    fn the_actions_of_a_folder_do_not_offer_the_preview() {
        let actions = Action::for_kind(IconKind::Folder);
        assert!(!actions.contains(&Action::Preview), "{actions:?}");
        // The other three are offered for every row.
        for action in [Action::Open, Action::OpenFolder, Action::CopyPath] {
            assert!(actions.contains(&action), "{action:?} is missing");
        }
    }

    /// An image, a video, a sound and a program are not text.
    ///
    /// The preview would show twelve lines of noise, and a menu that
    /// offers it is a menu that fails every time it is used.
    #[test]
    fn the_actions_of_a_binary_do_not_offer_the_preview() {
        for kind in [
            IconKind::Image,
            IconKind::Video,
            IconKind::Audio,
            IconKind::Application,
        ] {
            let actions = Action::for_kind(kind);
            assert!(!actions.contains(&Action::Preview), "{kind:?}: {actions:?}");
        }
    }

    /// A document and a plain file are text.
    ///
    /// The head of the file is what the user is looking for, and the
    /// preview is how they read it.
    #[test]
    fn the_actions_of_a_document_and_of_a_plain_file_offer_the_preview() {
        for kind in [IconKind::Document, IconKind::File] {
            assert!(
                Action::for_kind(kind).contains(&Action::Preview),
                "{kind:?} is text"
            );
        }
    }

    /// The menu is never one of the actions of a row.
    ///
    /// "Ações" opens the list, and a list that offers itself opens the
    /// menu the user is already in.
    #[test]
    fn the_actions_of_a_row_do_not_include_the_menu() {
        for kind in [
            IconKind::Folder,
            IconKind::Document,
            IconKind::Image,
            IconKind::Video,
            IconKind::Audio,
            IconKind::Application,
            IconKind::File,
        ] {
            assert!(!Action::for_kind(kind).contains(&Action::Menu), "{kind:?}");
        }
    }

    /// The refusal says why, in the user's language.
    ///
    /// A key that was pressed and refused in silence is a key the user
    /// presses again, and the sentence has to name the thing that was
    /// refused.
    #[test]
    fn the_refusal_says_why() {
        let reason = Action::Preview
            .refusal(IconKind::Folder)
            .expect("a folder has no preview");
        assert!(reason.contains("pasta"), "{reason}");
        assert!(reason.contains("pré-visualização"), "{reason}");
        // And what fits is not refused.
        assert!(Action::Preview.refusal(IconKind::Document).is_none());
        assert!(Action::Open.refusal(IconKind::Folder).is_none());
        assert!(Action::CopyPath.refusal(IconKind::Image).is_none());
    }
}
