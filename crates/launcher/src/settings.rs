// SPDX-License-Identifier: Apache-2.0

//! The launcher's settings, and the file that carries them.
//!
//! The settings are one answer to three questions — what the overlay
//! looks like, what the terminal looks like, what the picker looks like
//! — and the three answers are **independent**: a user who wants the
//! window warm and the terminal cool has them, on one line each.
//!
//! The file is line-delimited text, `key<TAB>value`, with a version
//! header, because that is the shape every store in this tree has: a
//! file a person can read in an editor and the program can rewrite
//! without losing the lines it does not know. A key nobody knows is
//! ignored — a newer file on an older launcher still loads — and a
//! value that cannot be read keeps the default, because a setting that
//! made the launcher refuse to start would be a setting the user could
//! not fix from the launcher.
//!
//! The panel and the file are the same store: the panel writes this
//! file, and a file edited by hand is read on the next start. There is
//! no second place the settings live.

use alloc::string::{String, ToString as _};
use alloc::vec::Vec;

use crate::theme::{Palette, Rgb, Theme};

/// The first line of the file, and its version.
pub const HEADER: &str = "# rgy settings v1";

/// The line under the header, saying what a line is.
const HEADER_NOTE: &str =
    "# key<TAB>value; a key nobody knows is ignored, a value nobody can read keeps the default";

/// Which modifier a double-tap watches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TapKey {
    /// Control, the launch's own: Ctrl Ctrl.
    #[default]
    Ctrl,
    /// Alt, twice.
    Alt,
    /// Shift, twice.
    Shift,
}

impl TapKey {
    /// The word the file and the panel use.
    pub fn name(self) -> &'static str {
        match self {
            TapKey::Ctrl => "ctrl",
            TapKey::Alt => "alt",
            TapKey::Shift => "shift",
        }
    }

    /// The modifier a word names, if it names one.
    pub fn named(word: &str) -> Option<TapKey> {
        Some(match word {
            "ctrl" => TapKey::Ctrl,
            "alt" => TapKey::Alt,
            "shift" => TapKey::Shift,
            _ => return None,
        })
    }

    /// The three, in the order the panel offers them.
    pub const ALL: &'static [TapKey] = &[TapKey::Ctrl, TapKey::Alt, TapKey::Shift];
}

/// The bits of a combo's modifiers.
///
/// The bits are the platform's own `MOD_*` values for `RegisterHotKey`:
/// a number the platform chose is a fact, and a translation table
/// between two spellings of it is a table that can drift.
pub mod mods {
    /// `MOD_ALT`.
    pub const ALT: u8 = 0x1;
    /// `MOD_CONTROL`.
    pub const CONTROL: u8 = 0x2;
    /// `MOD_SHIFT`.
    pub const SHIFT: u8 = 0x4;
    /// `MOD_WIN`.
    pub const WIN: u8 = 0x8;
}

/// The keys a combo can be completed with, by the word that names them.
///
/// The virtual-key code is the platform's own; the words are the file's
/// vocabulary, and a key nobody can name is a key nobody can ask for.
pub const COMBO_KEYS: &[(&str, u32)] = &[
    ("space", 0x20),
    ("enter", 0x0D),
    ("tab", 0x09),
    ("escape", 0x1B),
    ("up", 0x26),
    ("down", 0x28),
    ("left", 0x25),
    ("right", 0x27),
    ("a", 0x41),
    ("b", 0x42),
    ("c", 0x43),
    ("d", 0x44),
    ("e", 0x45),
    ("f", 0x46),
    ("g", 0x47),
    ("h", 0x48),
    ("i", 0x49),
    ("j", 0x4A),
    ("k", 0x4B),
    ("l", 0x4C),
    ("m", 0x4D),
    ("n", 0x4E),
    ("o", 0x4F),
    ("p", 0x50),
    ("q", 0x51),
    ("r", 0x52),
    ("s", 0x53),
    ("t", 0x54),
    ("u", 0x55),
    ("v", 0x56),
    ("w", 0x57),
    ("x", 0x58),
    ("y", 0x59),
    ("z", 0x5A),
    ("0", 0x30),
    ("1", 0x31),
    ("2", 0x32),
    ("3", 0x33),
    ("4", 0x34),
    ("5", 0x35),
    ("6", 0x36),
    ("7", 0x37),
    ("8", 0x38),
    ("9", 0x39),
    ("f1", 0x70),
    ("f2", 0x71),
    ("f3", 0x72),
    ("f4", 0x73),
    ("f5", 0x74),
    ("f6", 0x75),
    ("f7", 0x76),
    ("f8", 0x77),
    ("f9", 0x78),
    ("f10", 0x79),
    ("f11", 0x7A),
    ("f12", 0x7B),
    (";", 0xBA),
    ("=", 0xBB),
    (",", 0xBC),
    ("-", 0xBD),
    (".", 0xBE),
    ("/", 0xBF),
    ("`", 0xC0),
    ("[", 0xDB),
    ("\\", 0xDC),
    ("]", 0xDD),
    ("'", 0xDE),
];

/// The virtual-key code a word names, if it names one.
pub fn combo_key(word: &str) -> Option<u32> {
    COMBO_KEYS
        .iter()
        .find(|(known, _)| *known == word)
        .map(|(_, code)| *code)
}

/// The word that names a virtual-key code, if the table has one.
pub fn combo_key_name(code: u32) -> Option<&'static str> {
    COMBO_KEYS
        .iter()
        .find(|(_, known)| *known == code)
        .map(|(name, _)| *name)
}

/// The key that opens the launcher.
///
/// The two shapes are two mechanisms, and only one is armed at a time:
/// the **double tap** of a lone modifier, which the platform's own
/// hotkey registration cannot express and the low-level hook watches
/// for, and a **combo** of modifiers and a key, which is what
/// `RegisterHotKey` is for and needs no hook at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hotkey {
    /// A lone modifier, tapped twice inside a window of milliseconds.
    DoubleTap {
        /// Which modifier.
        key: TapKey,
        /// How long the window between the two taps is.
        window_ms: u32,
    },
    /// Modifiers held with one key.
    Combo {
        /// The modifiers, as [`mods`] bits. At least one: a combo
        /// without modifiers would be a key the user can never type
        /// anywhere else.
        modifiers: u8,
        /// The key, as a virtual-key code from [`COMBO_KEYS`].
        key: u32,
    },
}

impl Default for Hotkey {
    fn default() -> Hotkey {
        Hotkey::DoubleTap {
            key: TapKey::Ctrl,
            window_ms: 400,
        }
    }
}

impl Hotkey {
    /// The line the file carries, and the panel shows.
    pub fn spell(self) -> String {
        match self {
            Hotkey::DoubleTap { key, window_ms } => {
                alloc::format!("double {} {window_ms}", key.name())
            }
            Hotkey::Combo { modifiers, key } => {
                // The canonical spelling joins with `+` — the way a hand
                // writes it — and names the modifiers in one order, so
                // the file the panel writes is stable from one write to
                // the next.
                let mut said = String::from("combo");
                let mut first = true;
                for (bit, name) in [
                    (mods::CONTROL, "ctrl"),
                    (mods::ALT, "alt"),
                    (mods::SHIFT, "shift"),
                    (mods::WIN, "win"),
                ] {
                    if modifiers & bit != 0 {
                        said.push(if first { ' ' } else { '+' });
                        said.push_str(name);
                        first = false;
                    }
                }
                if let Some(name) = combo_key_name(key) {
                    said.push('+');
                    said.push_str(name);
                }
                said
            }
        }
    }

    /// The hotkey a line names, if it names one.
    ///
    /// `double ctrl` without a window keeps the default window, and the
    /// window is clamped the way the model clamps it on read. A combo
    /// names at least one modifier: `combo space` is not a combo, and a
    /// key the user types on its own is a key the launcher must not
    /// take.
    pub fn parse(text: &str) -> Option<Hotkey> {
        let words: Vec<&str> = text.split_whitespace().collect();
        match words.first().copied()? {
            "double" => {
                let key = TapKey::named(words.get(1).copied()?)?;
                let window_ms = match words.get(2) {
                    Some(number) => number
                        .trim()
                        .parse::<u32>()
                        .ok()?
                        .clamp(MIN_WINDOW_MS, MAX_WINDOW_MS),
                    None => match Hotkey::default() {
                        Hotkey::DoubleTap { window_ms, .. } => window_ms,
                        Hotkey::Combo { .. } => unreachable!("the default is a double tap"),
                    },
                };
                Some(Hotkey::DoubleTap { key, window_ms })
            }
            "combo" => {
                // The words are separated by spaces or by the `+` a
                // hand writes them with: `ctrl+shift+p` and
                // `ctrl shift p` name the same combo, and a file a
                // person edits accepts both.
                let mut modifiers = 0u8;
                let mut key = None;
                for word in words[1..]
                    .iter()
                    .flat_map(|word| word.split(['+', ' ']))
                    .filter(|word| !word.is_empty())
                {
                    let named = match word {
                        "ctrl" => Some(mods::CONTROL),
                        "alt" => Some(mods::ALT),
                        "shift" => Some(mods::SHIFT),
                        "win" => Some(mods::WIN),
                        _ => None,
                    };
                    match named {
                        Some(bit) => modifiers |= bit,
                        None => {
                            key = Some(combo_key(word)?);
                        }
                    }
                }
                let key = key?;
                if modifiers == 0 {
                    return None;
                }
                Some(Hotkey::Combo { modifiers, key })
            }
            _ => None,
        }
    }
}

/// The narrowest window a double tap can have.
const MIN_WINDOW_MS: u32 = 100;
/// The widest window a double tap can have.
const MAX_WINDOW_MS: u32 = 1500;
/// The narrowest the overlay can be, in model units.
const MIN_WIDTH: u32 = 400;
/// The widest the overlay can be, in model units.
const MAX_WIDTH: u32 = 1000;
/// The fewest rows the list can show.
const MIN_ROWS: u32 = 1;
/// The most rows the list can show.
const MAX_ROWS: u32 = 30;
/// The most transparent the panel can be.
///
/// Below this the window is a ghost: a thing on the screen the user
/// cannot read and cannot tell is there.
const MIN_ALPHA: u32 = 64;

/// The overlay's own settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlay {
    /// The colours it is drawn in.
    pub palette: Palette,
    /// How wide the window is, in model units.
    pub width: u32,
    /// How many rows the list shows at most.
    pub rows: u32,
    /// Where the window sits across the work area, in thousandths.
    ///
    /// Five hundred is the middle; zero is the left edge; a thousand is
    /// the right edge. The number is a permille and not a fraction
    /// because a settings file carries numbers, not dots.
    pub x: u32,
    /// Where the window sits down the work area, in thousandths.
    ///
    /// One hundred and eighty is where the launcher has always sat:
    /// near enough the top to be under the eye, far enough to leave the
    /// menu bar alone.
    pub y: u32,
    /// How opaque the panel is: zero is glass, two hundred and fifty
    /// five is stone.
    pub alpha: u32,
    /// The key that opens it.
    pub hotkey: Hotkey,
}

impl Default for Overlay {
    fn default() -> Overlay {
        Overlay {
            palette: Palette::on("warm"),
            width: 600,
            rows: 12,
            x: 500,
            y: 180,
            alpha: 230,
            hotkey: Hotkey::default(),
        }
    }
}

/// The picker's own settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fzf {
    /// The colours its flags are built from.
    pub palette: Palette,
    /// The arguments a launch adds to the picker's own, one per line of
    /// the file, in the order they were written.
    pub args: Vec<String>,
}

impl Default for Fzf {
    fn default() -> Fzf {
        Fzf {
            palette: Palette::on("warm"),
            args: Vec::new(),
        }
    }
}

/// All of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The overlay's face.
    pub overlay: Overlay,
    /// The terminal's face.
    pub tui: Palette,
    /// The picker's face and flags.
    pub fzf: Fzf,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            overlay: Overlay::default(),
            tui: Palette::on("midnight"),
            fzf: Fzf::default(),
        }
    }
}

impl Settings {
    /// The file that carries the settings.
    ///
    /// Every key on its own line, the palette overrides after the
    /// template they sit on, the free picker arguments last. The file
    /// the panel writes is the file a person can read.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(HEADER);
        out.push('\n');
        out.push_str(HEADER_NOTE);
        out.push('\n');
        render_palette(&mut out, "overlay", &self.overlay.palette);
        push(&mut out, "overlay.width", &self.overlay.width.to_string());
        push(&mut out, "overlay.rows", &self.overlay.rows.to_string());
        push(&mut out, "overlay.x", &self.overlay.x.to_string());
        push(&mut out, "overlay.y", &self.overlay.y.to_string());
        push(&mut out, "overlay.alpha", &self.overlay.alpha.to_string());
        push(&mut out, "hotkey", &self.overlay.hotkey.spell());
        render_palette(&mut out, "tui", &self.tui);
        render_palette(&mut out, "fzf", &self.fzf.palette);
        for arg in &self.fzf.args {
            push(&mut out, "fzf-arg", arg);
        }
        out
    }

    /// The settings a file carries.
    ///
    /// `None` is a file this version does not know: a header nobody
    /// recognises is a future file, and reading past it would be
    /// guessing. Inside a known header, a key nobody knows is ignored
    /// and a value nobody can read keeps the default.
    pub fn parse(text: &str) -> Option<Settings> {
        if !text.starts_with(HEADER) {
            return None;
        }
        let mut settings = Settings::default();
        for line in text.lines().skip_while(|line| line.starts_with('#')) {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('\t') else {
                continue;
            };
            settings.set(key, value.trim());
        }
        Some(settings)
    }

    /// Set one key, the way the file and the panel both do.
    ///
    /// The answer says whether the key was one anybody knows; a value
    /// that cannot be read is the default's, silently, because a file
    /// a person edits by hand must not have to be perfect.
    pub fn set(&mut self, key: &str, value: &str) -> bool {
        match key {
            "overlay.theme" | "tui.theme" | "fzf.theme" => {
                let palette = match key {
                    "overlay.theme" => &mut self.overlay.palette,
                    "tui.theme" => &mut self.tui,
                    _ => &mut self.fzf.palette,
                };
                palette.template = value.to_string();
                true
            }
            "overlay.width" => {
                self.overlay.width = number(value, MIN_WIDTH, MAX_WIDTH, 600);
                true
            }
            "overlay.rows" => {
                self.overlay.rows = number(value, MIN_ROWS, MAX_ROWS, 12);
                true
            }
            "overlay.x" => {
                self.overlay.x = number(value, 0, 1000, 500);
                true
            }
            "overlay.y" => {
                self.overlay.y = number(value, 0, 1000, 180);
                true
            }
            "overlay.alpha" => {
                self.overlay.alpha = number(value, MIN_ALPHA, 255, 230);
                true
            }
            "hotkey" => {
                if let Some(hotkey) = Hotkey::parse(value) {
                    self.overlay.hotkey = hotkey;
                }
                true
            }
            "fzf-arg" => {
                self.fzf.args.push(value.to_string());
                true
            }
            _ if key.starts_with("overlay.colour.")
                || key.starts_with("tui.colour.")
                || key.starts_with("fzf.colour.") =>
            {
                let Some((_, role)) = key.split_once(".colour.") else {
                    return false;
                };
                let palette = match key.split_once('.').map(|(face, _)| face) {
                    Some("overlay") => &mut self.overlay.palette,
                    Some("tui") => &mut self.tui,
                    Some("fzf") => &mut self.fzf.palette,
                    _ => return false,
                };
                match Rgb::parse(value) {
                    Some(colour) => palette.set_by_name(role, colour).is_some(),
                    None => role_names_have(palette, role),
                }
            }
            _ => false,
        }
    }
}

/// Whether a palette already carries an override for a role — used only
/// to answer "was this a key anybody knows" for a colour that could not
/// be read.
fn role_names_have(palette: &Palette, role: &str) -> bool {
    Theme::role_names().contains(&role) && palette.overrides.iter().any(|(known, _)| *known == role)
}

/// A number, clamped to the range the model holds.
///
/// A value that cannot be read is the default's: the file does not have
/// to be perfect, and a setting the launcher cannot read is a setting
/// the launcher still has.
fn number(value: &str, min: u32, max: u32, default: u32) -> u32 {
    value
        .trim()
        .parse::<u32>()
        .ok()
        .unwrap_or(default)
        .clamp(min, max)
}

/// One line of the file.
fn push(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push('\t');
    out.push_str(value);
    out.push('\n');
}

/// The lines a face's palette is written as.
fn render_palette(out: &mut String, face: &str, palette: &Palette) {
    push(out, &alloc::format!("{face}.theme"), &palette.template);
    for (role, colour) in &palette.overrides {
        push(out, &alloc::format!("{face}.colour.{role}"), &colour.hex());
    }
}

/// The picker's colour flags, built from a theme.
///
/// The roles map to the picker's own names, once, here: `fg` is the
/// text, `bg` the background, `hl` the matched span, `pointer` the
/// accent's job, `border` the border. One flag carries them all, the
/// way the picker takes them, and a caller that passes them to a picker
/// which is not `fzf` gets nothing it can read — the caller decides
/// that, not this function.
pub fn fzf_colour_args(theme: &Theme) -> Vec<String> {
    let pairs: [(&str, Rgb); 14] = [
        ("fg", theme.text),
        ("fg+", theme.text),
        ("bg", theme.background),
        ("bg+", theme.selected),
        ("hl", theme.matched),
        ("hl+", theme.matched),
        ("pointer", theme.accent),
        ("marker", theme.matched),
        ("spinner", theme.good),
        ("info", theme.dim),
        ("header", theme.dim),
        ("border", theme.border),
        ("prompt", theme.accent),
        ("gutter", theme.background),
    ];
    let mut spec = String::from("--color=");
    for (i, (name, colour)) in pairs.iter().enumerate() {
        if i > 0 {
            spec.push(',');
        }
        spec.push_str(name);
        spec.push(':');
        spec.push_str(&colour.hex());
    }
    Vec::from([spec])
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// The defaults are the faces as they are today.
    ///
    /// A settings file that does not exist must change nothing: the
    /// window keeps its width and its seat, the hotkey keeps the
    /// gesture, and the three faces keep their own palettes.
    #[test]
    fn the_defaults_are_the_faces_as_they_are_today() {
        let settings = Settings::default();
        assert_eq!(settings.overlay.palette.template, "warm");
        assert_eq!(settings.tui.template, "midnight");
        assert_eq!(settings.fzf.palette.template, "warm");
        assert_eq!(settings.overlay.width, 600);
        assert_eq!(settings.overlay.rows, 12);
        assert_eq!(settings.overlay.x, 500);
        assert_eq!(settings.overlay.y, 180);
        assert_eq!(settings.overlay.alpha, 230);
        assert_eq!(
            settings.overlay.hotkey,
            Hotkey::DoubleTap {
                key: TapKey::Ctrl,
                window_ms: 400
            }
        );
        assert!(settings.fzf.args.is_empty());
    }

    /// The file round-trips.
    #[test]
    fn the_file_round_trips() {
        let mut settings = Settings::default();
        settings.overlay.palette.template = "nord".to_string();
        settings
            .overlay
            .palette
            .set_by_name("text", Rgb { r: 1, g: 2, b: 3 })
            .expect("text is a role");
        settings.tui.template = "light".to_string();
        settings.fzf.args = vec!["--height".to_string(), "80%".to_string()];
        settings.overlay.hotkey = Hotkey::Combo {
            modifiers: mods::CONTROL | mods::ALT,
            key: combo_key("p").expect("p is a key"),
        };
        let file = settings.render();
        let back = Settings::parse(&file).expect("the file parses");
        assert_eq!(back, settings, "the file carries everything");
    }

    /// A file without the header is a file nobody knows.
    #[test]
    fn a_file_without_the_header_is_a_file_nobody_knows() {
        assert!(Settings::parse("overlay.width\t700\n").is_none());
        assert!(Settings::parse("").is_none());
    }

    /// A key nobody knows is ignored, and everything else still loads.
    #[test]
    fn a_key_nobody_knows_is_ignored() {
        let file = "# rgy settings v1\n# note\noverlay.width\t700\na.key.from.the.future\t1\n";
        let settings = Settings::parse(file).expect("the file parses");
        assert_eq!(settings.overlay.width, 700);
    }

    /// A value nobody can read keeps the default.
    #[test]
    fn a_value_nobody_can_read_keeps_the_default() {
        let file =
            "# rgy settings v1\noverlay.width\tnarrow\noverlay.rows\tmany\nhotkey\tbicycle\n";
        let settings = Settings::parse(file).expect("the file parses");
        assert_eq!(settings.overlay.width, 600);
        assert_eq!(settings.overlay.rows, 12);
        assert_eq!(settings.overlay.hotkey, Hotkey::default());
    }

    /// The numbers are clamped to the ranges the model holds.
    #[test]
    fn the_numbers_are_clamped() {
        let file = "# rgy settings v1\noverlay.width\t10\noverlay.rows\t999\noverlay.x\t2000\noverlay.alpha\t0\nhotkey\tdouble ctrl 5000\n";
        let settings = Settings::parse(file).expect("the file parses");
        assert_eq!(settings.overlay.width, MIN_WIDTH);
        assert_eq!(settings.overlay.rows, MAX_ROWS);
        assert_eq!(settings.overlay.x, 1000);
        assert_eq!(settings.overlay.alpha, MIN_ALPHA);
        assert_eq!(
            settings.overlay.hotkey,
            Hotkey::DoubleTap {
                key: TapKey::Ctrl,
                window_ms: MAX_WINDOW_MS
            }
        );
    }

    /// The hotkey spells and parses in both shapes.
    #[test]
    fn the_hotkey_spells_and_parses_in_both_shapes() {
        let double = Hotkey::DoubleTap {
            key: TapKey::Shift,
            window_ms: 350,
        };
        assert_eq!(double.spell(), "double shift 350");
        assert_eq!(Hotkey::parse("double shift 350"), Some(double));
        // The window is optional in the file and the default's in it.
        assert_eq!(
            Hotkey::parse("double shift"),
            Some(Hotkey::DoubleTap {
                key: TapKey::Shift,
                window_ms: 400
            })
        );

        let combo = Hotkey::Combo {
            modifiers: mods::ALT,
            key: combo_key("space").expect("space is a key"),
        };
        assert_eq!(combo.spell(), "combo alt+space");
        assert_eq!(Hotkey::parse("combo alt+space"), Some(combo));

        let many = Hotkey::Combo {
            modifiers: mods::CONTROL | mods::SHIFT,
            key: combo_key("p").expect("p is a key"),
        };
        assert_eq!(many.spell(), "combo ctrl+shift+p");
        assert_eq!(Hotkey::parse("combo ctrl+shift+p"), Some(many));
        assert_eq!(Hotkey::parse("combo shift+ctrl+p"), Some(many));
    }

    /// A combo with no modifier is not a combo.
    ///
    /// `combo space` would take the space bar away from every program
    /// on the machine, and a launcher that did that is a launcher the
    /// user uninstalls.
    #[test]
    fn a_combo_with_no_modifier_is_not_a_combo() {
        assert_eq!(Hotkey::parse("combo space"), None);
        assert_eq!(Hotkey::parse("combo"), None);
        assert_eq!(Hotkey::parse("double capslock"), None);
        assert_eq!(Hotkey::parse("triple ctrl"), None);
    }

    /// The modifier bits are the platform's own.
    ///
    /// They are handed to `RegisterHotKey` as they are read here, and a
    /// translation between two spellings of the platform's numbers is a
    /// translation that can drift.
    #[test]
    fn the_modifier_bits_are_the_platform_s_own() {
        assert_eq!(mods::ALT, 0x1);
        assert_eq!(mods::CONTROL, 0x2);
        assert_eq!(mods::SHIFT, 0x4);
        assert_eq!(mods::WIN, 0x8);
    }

    /// The free picker arguments are the file's lines, in order, and a
    /// repeated scalar keeps the last.
    #[test]
    fn the_free_picker_arguments_are_the_file_s_lines() {
        let file = "# rgy settings v1\nfzf-arg\t--height\nfzf-arg\t80%\noverlay.width\t700\noverlay.width\t800\n";
        let settings = Settings::parse(file).expect("the file parses");
        assert_eq!(settings.fzf.args, vec!["--height", "80%"]);
        assert_eq!(settings.overlay.width, 800);
    }

    /// A colour a file names sits on the face it was named for.
    #[test]
    fn a_colour_a_file_names_sits_on_the_face_it_was_named_for() {
        let file = "# rgy settings v1\ntui.theme\tmidnight\ntui.colour.text\t#ff0000\n";
        let settings = Settings::parse(file).expect("the file parses");
        let theme = settings.tui.theme(Theme::MIDNIGHT);
        assert_eq!(theme.text, Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(theme.background, Theme::MIDNIGHT.background);
    }

    /// The picker's colour flag is one flag, with every role in it.
    #[test]
    fn the_picker_s_colour_flag_is_one_flag() {
        let args = fzf_colour_args(&Theme::WARM);
        assert_eq!(args.len(), 1, "one flag: {args:?}");
        assert!(args[0].starts_with("--color=fg:"), "{}", args[0]);
        assert!(args[0].contains("pointer:#5a96ff"), "{}", args[0]);
        assert!(args[0].contains("bg:#141518"), "{}", args[0]);
        assert_eq!(args[0].matches(',').count(), 13, "{}", args[0]);
    }
}
