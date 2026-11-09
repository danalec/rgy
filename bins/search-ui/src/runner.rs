// SPDX-License-Identifier: Apache-2.0

//! The machine behind the terminal's actions.
//!
//! The model decides what the user asked for; this is what carries it out.
//! The window has `rgy_launcher_windows::actions` and
//! `rgy_launcher_x11::actions` behind it, and the terminal had nothing:
//! `>copy` printed the path and said the terminal owns no clipboard,
//! `>folder` printed the folder and said it cannot open one, and
//! `>settings` printed where the file is. That is a launcher that names
//! its options and then declines them.
//!
//! [`Runner`] is the seam the caller fills (`rgxy` implements it in its
//! own `tui_actions.rs`). `rgxy` hands the terminal the
//! same platform actions its window uses, plus its settings store, so the
//! two faces do the same things to the same files. A terminal started
//! without a runner keeps the old, honest notices: the words say what it
//! really did, and a face that cannot carry an action out must not claim
//! it did.
//!
//! The trait lives here and not in the model because the model is
//! `no_std` and knows nothing about a machine.

use rgy_launcher::Settings;

/// What the terminal asks the machine to do.
pub trait Runner {
    /// Whether this runner really carries actions out.
    ///
    /// [`NoRunner`] says no, so a terminal started without a machine keeps
    /// the notices that describe what it can do instead of reporting a
    /// failure for a call it never made.
    fn available(&self) -> bool {
        true
    }

    /// Open a path with the platform's default program.
    fn open(&self, path: &str) -> Result<(), String>;

    /// Open the folder that holds a path, with the file selected where the
    /// platform can select it.
    fn open_folder(&self, path: &str) -> Result<(), String>;

    /// Put a path on the clipboard.
    fn copy_path(&self, path: &str) -> Result<(), String>;

    /// The clipboard's text, when the machine can read it.
    ///
    /// `None` is "no text", not an error: a terminal whose clipboard cannot
    /// be read still receives a paste from its own bracketed-paste bytes,
    /// and the key says so rather than inventing an empty paste.
    fn paste_text(&self) -> Option<String>;

    /// The settings, as the file has them.
    fn settings(&self) -> Settings;

    /// Write the settings back to the file.
    fn save_settings(&self, settings: &Settings) -> Result<(), String>;
}

/// A terminal that owns no machine: the old notices stay true.
///
/// Every call answers with a refusal, and [`Runner::available`] says so
/// before any of them is made. It is the default for the entry points that
/// take no runner, so a caller that does not have a platform behind it is
/// not a caller that suddenly claims to open files.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRunner;

impl Runner for NoRunner {
    fn available(&self) -> bool {
        false
    }

    fn open(&self, _path: &str) -> Result<(), String> {
        Err(String::from("no runner"))
    }

    fn open_folder(&self, _path: &str) -> Result<(), String> {
        Err(String::from("no runner"))
    }

    fn copy_path(&self, _path: &str) -> Result<(), String> {
        Err(String::from("no runner"))
    }

    fn paste_text(&self) -> Option<String> {
        None
    }

    fn settings(&self) -> Settings {
        Settings::default()
    }

    fn save_settings(&self, _settings: &Settings) -> Result<(), String> {
        Err(String::from("no runner"))
    }
}
