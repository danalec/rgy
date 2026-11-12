// SPDX-License-Identifier: Apache-2.0

//! The colours the launcher is drawn in.
//!
//! One vocabulary of **roles**, three faces that speak it: the window
//! paints its eight colours from it, the terminal interface its six, and
//! the picker's flags are built from it. A theme is the same answer to
//! the same question everywhere — what a name is written in, what the
//! matched characters are, what says "this one is fine" — so a palette
//! a user chose once follows the search from the overlay to the
//! terminal to the picker.
//!
//! The roles are a **superset**: the window has `selected` and `border`
//! that the terminal's reverse video stands in for, and the terminal has
//! nothing the window lacks. A face that does not use a role ignores it.
//!
//! A colour is RGB, because that is what both worlds take: the window's
//! `COLORREF` is RGB byte-swapped, and the terminal's SGR takes
//! `2;r;g;b`. Hex without the `#` is the file's spelling, because a
//! line a user edits by hand has no room for decoration.

use alloc::string::{String, ToString as _};
use alloc::vec::Vec;

/// One colour, in the order every spelling of it starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
}

impl Rgb {
    /// The colour as six hex digits, `rrggbb`, without the `#`.
    ///
    /// This is the file's spelling and the picker's: one form a user can
    /// type in a line editor and read back in the settings file.
    pub fn hex(self) -> String {
        alloc::format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// The colour as the window's own 0x00BBGGRR.
    ///
    /// The platform's colour is blue-first and the theme's is red-first,
    /// and a conversion written twice is a conversion that drifts.
    pub fn bgr(self) -> u32 {
        (self.b as u32) << 16 | (self.g as u32) << 8 | self.r as u32
    }

    /// The colour a hex spelling names, with or without the `#`.
    pub fn parse(text: &str) -> Option<Rgb> {
        let text = text.trim().trim_start_matches('#');
        if text.len() != 6 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let channel = |from: usize| u8::from_str_radix(&text[from..from + 2], 16).ok();
        Some(Rgb {
            r: channel(0)?,
            g: channel(2)?,
            b: channel(4)?,
        })
    }
}

/// The roles a face is drawn in.
///
/// Every role has a job, and the jobs are the same whichever face asks:
/// `text` is what the user reads first, `dim` is what they read around
/// it, `matched` is what the query earned, `accent` is the machine's own
/// mark (the selection's bar, the prompt), `good` is the number that
/// says the machine is fine, and `background`, `selected` and `border`
/// are the surfaces those sit on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// What the window is made of: the panel, the screen behind
    /// everything.
    pub background: Rgb,
    /// Names, the query, everything the user is looking for.
    pub text: Rgb,
    /// Folders, the status line, the words around the answer.
    pub dim: Rgb,
    /// The machine's own mark: the selection's bar, the prompt, the
    /// caret.
    pub accent: Rgb,
    /// The characters the query matched: the span that earned the score.
    pub matched: Rgb,
    /// The figure that says the machine is fine: the latency, the meter.
    pub good: Rgb,
    /// The band a selected row sits on.
    pub selected: Rgb,
    /// The hairline at the edge of things.
    pub border: Rgb,
}

impl Default for Theme {
    fn default() -> Theme {
        // The window's own: the theme a view carries before anyone chose
        // one is the one the window was drawn in before themes existed.
        Theme::WARM
    }
}

impl Theme {
    /// The window's own palette, the one it was drawn in before themes
    /// existed: warm near-whites on a warm near-black, the blue of the
    /// matches and the green of the meter.
    pub const WARM: Theme = Theme {
        background: Rgb {
            r: 0x14,
            g: 0x15,
            b: 0x18,
        },
        text: Rgb {
            r: 0xF0,
            g: 0xF2,
            b: 0xF5,
        },
        dim: Rgb {
            r: 0x82,
            g: 0x87,
            b: 0x91,
        },
        accent: Rgb {
            r: 0x5A,
            g: 0x96,
            b: 0xFF,
        },
        matched: Rgb {
            r: 0x78,
            g: 0xB4,
            b: 0xFF,
        },
        good: Rgb {
            r: 0x77,
            g: 0xD9,
            b: 0x8B,
        },
        selected: Rgb {
            r: 0x26,
            g: 0x28,
            b: 0x2E,
        },
        border: Rgb {
            r: 0x3A,
            g: 0x3C,
            b: 0x42,
        },
    };

    /// The terminal's own palette, from before themes existed: the
    /// charcoal background, the white ink, the periwinkle prompt and the
    /// amber matches of the recording the interface was drawn after.
    pub const MIDNIGHT: Theme = Theme {
        background: Rgb {
            r: 19,
            g: 18,
            b: 21,
        },
        text: Rgb {
            r: 255,
            g: 255,
            b: 255,
        },
        dim: Rgb {
            r: 115,
            g: 114,
            b: 119,
        },
        accent: Rgb {
            r: 114,
            g: 170,
            b: 255,
        },
        matched: Rgb {
            r: 233,
            g: 184,
            b: 92,
        },
        good: Rgb {
            r: 119,
            g: 217,
            b: 139,
        },
        selected: Rgb {
            r: 38,
            g: 37,
            b: 42,
        },
        border: Rgb {
            r: 58,
            g: 56,
            b: 62,
        },
    };

    /// A cool dark: blue-leaning blacks, cyan marks.
    pub const DARK_COOL: Theme = Theme {
        background: Rgb {
            r: 13,
            g: 16,
            b: 22,
        },
        text: Rgb {
            r: 0xE8,
            g: 0xEE,
            b: 0xF8,
        },
        dim: Rgb {
            r: 0x7C,
            g: 0x88,
            b: 0x9C,
        },
        accent: Rgb {
            r: 86,
            g: 182,
            b: 224,
        },
        matched: Rgb {
            r: 0x9A,
            g: 0xE6,
            b: 0xFF,
        },
        good: Rgb {
            r: 0x6E,
            g: 0xE8,
            b: 0xB0,
        },
        selected: Rgb {
            r: 22,
            g: 28,
            b: 38,
        },
        border: Rgb {
            r: 36,
            g: 44,
            b: 58,
        },
    };

    /// A light: paper for a panel, dark ink for the words.
    pub const LIGHT: Theme = Theme {
        background: Rgb {
            r: 0xF2,
            g: 0xF0,
            b: 0xEB,
        },
        text: Rgb {
            r: 0x28,
            g: 0x28,
            b: 0x2A,
        },
        dim: Rgb {
            r: 0x78,
            g: 0x74,
            b: 0x6E,
        },
        accent: Rgb {
            r: 0x2E,
            g: 0x62,
            b: 0xD4,
        },
        matched: Rgb {
            r: 0x9A,
            g: 0x5B,
            b: 0x00,
        },
        good: Rgb {
            r: 0x2E,
            g: 0x8B,
            b: 0x57,
        },
        selected: Rgb {
            r: 0xDC,
            g: 0xD8,
            b: 0xD0,
        },
        border: Rgb {
            r: 0xB8,
            g: 0xB2,
            b: 0xA8,
        },
    };

    /// Black on white with the marks at full saturation: for the screen
    /// the sun is on.
    pub const HIGH_CONTRAST: Theme = Theme {
        background: Rgb {
            r: 0x00,
            g: 0x00,
            b: 0x00,
        },
        text: Rgb {
            r: 0xFF,
            g: 0xFF,
            b: 0xFF,
        },
        dim: Rgb {
            r: 0xB4,
            g: 0xB4,
            b: 0xB4,
        },
        accent: Rgb {
            r: 0x00,
            g: 0xFF,
            b: 0xFF,
        },
        matched: Rgb {
            r: 0xFF,
            g: 0xFF,
            b: 0x00,
        },
        good: Rgb {
            r: 0x00,
            g: 0xFF,
            b: 0x00,
        },
        selected: Rgb {
            r: 0x40,
            g: 0x40,
            b: 0x40,
        },
        border: Rgb {
            r: 0x80,
            g: 0x80,
            b: 0x80,
        },
    };

    /// The Nord palette, for the desktop that wears it.
    pub const NORD: Theme = Theme {
        background: Rgb {
            r: 46,
            g: 52,
            b: 64,
        },
        text: Rgb {
            r: 236,
            g: 239,
            b: 244,
        },
        dim: Rgb {
            r: 143,
            g: 155,
            b: 175,
        },
        accent: Rgb {
            r: 136,
            g: 192,
            b: 208,
        },
        matched: Rgb {
            r: 235,
            g: 203,
            b: 139,
        },
        good: Rgb {
            r: 163,
            g: 190,
            b: 140,
        },
        selected: Rgb {
            r: 59,
            g: 66,
            b: 82,
        },
        border: Rgb {
            r: 76,
            g: 86,
            b: 106,
        },
    };

    /// The named themes, in the order the settings panel offers them.
    ///
    /// The first two are the faces' own: `warm` is what the window was
    /// drawn in before themes existed, `midnight` what the terminal was.
    /// Every name is available to every face — a theme is an answer, not
    /// a face.
    pub const TEMPLATES: &[(&str, Theme)] = &[
        ("warm", Theme::WARM),
        ("midnight", Theme::MIDNIGHT),
        ("dark-cool", Theme::DARK_COOL),
        ("light", Theme::LIGHT),
        ("high-contrast", Theme::HIGH_CONTRAST),
        ("nord", Theme::NORD),
    ];

    /// The theme a name names, if it names one.
    pub fn named(name: &str) -> Option<Theme> {
        Self::TEMPLATES
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, theme)| *theme)
    }

    /// The names the settings panel offers, in order.
    pub fn names() -> Vec<&'static str> {
        Self::TEMPLATES.iter().map(|(name, _)| *name).collect()
    }

    /// The names of the roles, in the order the struct declares them.
    ///
    /// The names are the file's vocabulary: `overlay.colour.text` is the
    /// `text` role of the overlay's theme, and a role a file cannot name
    /// is a role a file cannot override.
    pub fn role_names() -> &'static [&'static str] {
        &[
            "background",
            "text",
            "dim",
            "accent",
            "matched",
            "good",
            "selected",
            "border",
        ]
    }
}

/// A face's theme: the template it is built from, and the roles the user
/// moved on top of it.
///
/// The template is the starting point and the overrides are the point
/// the user made: a palette that is one name plus a handful of colours
/// is a palette a settings file can carry in a few lines, and a settings
/// panel can adjust one role at a time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Palette {
    /// The template's name. A name that names no template is the
    /// default's: a file that names a theme nobody has is a file that
    /// still has to draw.
    pub template: String,
    /// The roles the user moved, by role name.
    pub overrides: Vec<(&'static str, Rgb)>,
}

impl Palette {
    /// A palette on the named template, with nothing moved.
    pub fn on(template: &str) -> Palette {
        Palette {
            template: template.to_string(),
            overrides: Vec::new(),
        }
    }

    /// Move one role, by the role's name.
    ///
    /// The names are [`Theme::role_names`]: a role the vocabulary does
    /// not have is not moved, and the caller is told so. Setting a role
    /// twice keeps the last move, the way a file's last line wins.
    pub fn set_by_name(&mut self, role: &str, colour: Rgb) -> Option<()> {
        if !Theme::role_names().contains(&role) {
            return None;
        }
        let role = Theme::role_names()
            .iter()
            .find(|known| **known == role)
            .copied()?;
        self.overrides.retain(|(known, _)| *known != role);
        self.overrides.push((role, colour));
        Some(())
    }

    /// The theme this palette is: the template with the overrides
    /// applied on top.
    ///
    /// A name that names nothing answers the default: the question is
    /// "what does this face look like", and a face always looks like
    /// something.
    pub fn theme(&self, default: Theme) -> Theme {
        let mut theme = Theme::named(&self.template).unwrap_or(default);
        for (role, colour) in &self.overrides {
            let colour = *colour;
            match *role {
                "background" => theme.background = colour,
                "text" => theme.text = colour,
                "dim" => theme.dim = colour,
                "accent" => theme.accent = colour,
                "matched" => theme.matched = colour,
                "good" => theme.good = colour,
                "selected" => theme.selected = colour,
                "border" => theme.border = colour,
                _ => {}
            }
        }
        theme
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window's palette is the one it was drawn in.
    ///
    /// The constants of the window's picture are the theme's first duty:
    /// a theme that changed the colours nobody asked about is a theme
    /// that redraws the launcher under its users.
    #[test]
    fn the_window_s_palette_is_the_one_it_was_drawn_in() {
        assert_eq!(Theme::WARM.text.bgr(), 0x00F5_F2F0);
        assert_eq!(Theme::WARM.dim.bgr(), 0x0091_8782);
        assert_eq!(Theme::WARM.matched.bgr(), 0x00FF_B478);
        assert_eq!(Theme::WARM.selected.bgr(), 0x002E_2826);
        assert_eq!(Theme::WARM.accent.bgr(), 0x00FF_965A);
        assert_eq!(Theme::WARM.good.bgr(), 0x008B_D977);
        assert_eq!(Theme::WARM.background.bgr(), 0x0018_1514);
        assert_eq!(Theme::WARM.border.bgr(), 0x0042_3C3A);
    }

    /// The terminal's palette is the one it was drawn in.
    #[test]
    fn the_terminal_s_palette_is_the_one_it_was_drawn_in() {
        assert_eq!(
            Theme::MIDNIGHT.background,
            Rgb {
                r: 19,
                g: 18,
                b: 21
            }
        );
        assert_eq!(
            Theme::MIDNIGHT.accent,
            Rgb {
                r: 114,
                g: 170,
                b: 255
            }
        );
        assert_eq!(
            Theme::MIDNIGHT.matched,
            Rgb {
                r: 233,
                g: 184,
                b: 92
            }
        );
    }

    /// Every template's name names it, and no two share a name.
    #[test]
    fn every_template_s_name_names_it() {
        for (name, theme) in Theme::TEMPLATES {
            assert_eq!(
                Theme::named(name),
                Some(*theme),
                "the name {name} names its own template"
            );
        }
        let names = Theme::names();
        for name in &names {
            assert_eq!(
                names.iter().filter(|other| other == &name).count(),
                1,
                "the name {name} is the only one: {names:?}"
            );
        }
    }

    /// A hex colour round-trips.
    #[test]
    fn a_hex_colour_round_trips() {
        let colour = Rgb {
            r: 0xF0,
            g: 0xB4,
            b: 0x2A,
        };
        let said = colour.hex();
        assert_eq!(said, "#f0b42a", "the hex is six digits, lower case");
        assert_eq!(Rgb::parse(&said), Some(colour));
        assert_eq!(Rgb::parse("f0b42a"), Some(colour), "the # is optional");
        assert_eq!(Rgb::parse("#F0B42A"), Some(colour), "the case is free");
    }

    /// A colour that is not six hex digits is not a colour.
    #[test]
    fn a_colour_that_is_not_six_hex_digits_is_not_a_colour() {
        assert_eq!(Rgb::parse(""), None);
        assert_eq!(Rgb::parse("#fff"), None);
        assert_eq!(Rgb::parse("toomanydigits"), None);
        assert_eq!(Rgb::parse("zzzzzz"), None);
    }

    /// The overrides sit on the template, by role.
    #[test]
    fn the_overrides_sit_on_the_template() {
        let mut palette = Palette::on("midnight");
        assert_eq!(
            palette.theme(Theme::WARM),
            Theme::MIDNIGHT,
            "a palette with nothing moved is its template"
        );
        palette.overrides.push(("text", Rgb { r: 1, g: 2, b: 3 }));
        let theme = palette.theme(Theme::WARM);
        assert_eq!(theme.text, Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(theme.background, Theme::MIDNIGHT.background);
    }

    /// A template nobody has answers the default.
    #[test]
    fn a_template_nobody_has_answers_the_default() {
        let palette = Palette::on("a theme that does not exist");
        assert_eq!(palette.theme(Theme::WARM), Theme::WARM);
    }

    /// Every role a file can name is a role the theme carries.
    #[test]
    fn every_role_a_file_can_name_is_a_role_the_theme_carries() {
        for role in Theme::role_names() {
            assert!(
                Palette::set_by_name(&mut Palette::on("warm"), role, Rgb { r: 0, g: 0, b: 0 })
                    .is_some(),
                "the role {role} is a role a file can name"
            );
        }
        assert!(
            Palette::set_by_name(
                &mut Palette::on("warm"),
                "not-a-role",
                Rgb { r: 0, g: 0, b: 0 }
            )
            .is_none()
        );
    }
}
