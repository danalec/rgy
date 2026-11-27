// SPDX-License-Identifier: Apache-2.0

//! `settings.json`: the hand-edited layer over `settings.tsv`.
//!
//! The file is JSON with comments and trailing commas: one flat object
//! whose keys are the names `settings.tsv` already uses. A key it sets
//! wins over the stored value and is read-only to the panels
//! ([`Overrides`]). **The launcher never rewrites this file**: it writes
//! it once, with every setting commented out, when there is none, and
//! after that the comments, the layout and the keys it does not know are
//! the user's.
//!
//! The reading is the protocol's own JSON reader. The only thing added
//! here is the pass that takes comments and trailing commas out first.

use std::io::{Read as _, Write as _};
use std::path::Path;

use rgy_launcher::settings::{Hotkey, Settings};
use rgy_launcher::{Overrides, Rgb, Theme};
use rgy_proto::{Json, escape_json_string, read_json};

/// The version of the object this launcher reads.
const VERSION: f64 = 1.0;

/// The largest file that is read: a settings file is a few kilobytes.
const MAX_BYTES: u64 = 256 * 1024;

/// What the file says.
#[derive(Debug, Default)]
pub struct File {
    /// The keys it sets, with their values.
    pub held: Overrides,
    /// What could not be taken from it, one sentence each.
    pub problems: Vec<String>,
}

impl File {
    /// A file nothing is taken from, and why.
    fn ignored(reason: &str) -> File {
        File {
            held: Overrides::default(),
            problems: vec![format!("settings.json was ignored: {reason}")],
        }
    }
}

/// Read the file. One that is not there sets nothing and is no problem.
pub fn read(path: &Path) -> File {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return File::default(),
        Err(err) => return File::ignored(&err.to_string()),
    };
    let mut text = String::new();
    if let Err(err) = file.take(MAX_BYTES + 1).read_to_string(&mut text) {
        return File::ignored(&err.to_string());
    }
    if text.len() as u64 > MAX_BYTES {
        return File::ignored("it is larger than 256 KiB");
    }
    parse(&text)
}

/// What a text says.
///
/// A text that is not a JSON object, or that names another version, sets
/// nothing. Inside a good object each key stands alone: one that cannot
/// be taken is reported and the others still apply.
pub fn parse(text: &str) -> File {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(plain) = plain_json(text) else {
        return File::ignored("a /* comment is not closed");
    };
    let fields = match read_json(&plain) {
        Some(Json::Obj(fields)) => fields,
        Some(_) => return File::ignored("it is not an object"),
        None => return File::ignored("it is not valid JSON with comments"),
    };
    if fields
        .iter()
        .any(|(key, value)| key == "version" && value.as_num() != Some(VERSION))
    {
        return File::ignored("this launcher reads \"version\": 1");
    }
    let mut file = File::default();
    for (key, value) in &fields {
        if key == "version" || key == "$schema" {
            continue;
        }
        if file.held.holds(key) {
            file.problems.push(format!(
                "settings.json: {key} is set more than once; the last good value wins"
            ));
        }
        if let Err(reason) = take(&mut file, key, value) {
            file.problems.push(format!("settings.json: {key} {reason}"));
        }
    }
    file
}

/// Take one key into the file, or say why not.
fn take(file: &mut File, key: &str, value: &Json) -> Result<(), String> {
    match key {
        "overlay.theme" | "tui.theme" | "fzf.theme" => {
            let name = string(value)?;
            if Theme::named(name).is_none() {
                return Err(format!("names no theme: {}", Theme::names().join(", ")));
            }
            file.held.set(key, name);
        }
        "overlay.width" | "overlay.rows" | "overlay.x" | "overlay.y" | "overlay.alpha" => {
            let number = value
                .as_num()
                .filter(|n| n.fract() == 0.0 && *n >= 0.0 && *n <= f64::from(u32::MAX))
                .ok_or("must be a whole number, zero or more, without quotes")?
                as u32;
            file.held.set(key, &number.to_string());
            // The model clamps; a value that moved is worth a sentence.
            let mut probe = Settings::default();
            file.held.apply(&mut probe);
            let overlay = probe.overlay;
            let kept = match key {
                "overlay.width" => overlay.width,
                "overlay.rows" => overlay.rows,
                "overlay.x" => overlay.x,
                "overlay.y" => overlay.y,
                _ => overlay.alpha,
            };
            if kept != number {
                return Err(format!("is out of range; {kept} is used"));
            }
        }
        "hotkey" => {
            let spelled = string(value)?;
            if Hotkey::parse(spelled).is_none() {
                return Err("is not a shortcut such as \"combo alt+space\"".to_string());
            }
            file.held.set(key, spelled);
        }
        "fzf-arg" => {
            let args = value
                .as_array()
                .and_then(|items| {
                    items
                        .iter()
                        .map(|item| item.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or("must be a list of strings")?;
            file.held.set_fzf_args(args);
        }
        _ => {
            let known = key.split_once(".colour.").is_some_and(|(face, role)| {
                matches!(face, "overlay" | "tui" | "fzf") && Theme::role_names().contains(&role)
            });
            if !known {
                return Err("is not a key this launcher knows".to_string());
            }
            let colour = string(value)?;
            if Rgb::parse(colour).is_none() {
                return Err("must be a colour such as \"#1a2b3c\"".to_string());
            }
            file.held.set(key, colour);
        }
    }
    Ok(())
}

fn string(value: &Json) -> Result<&str, String> {
    value.as_str().ok_or_else(|| "must be a string".to_string())
}

/// The text with its comments and trailing commas replaced by spaces.
///
/// Strings are copied untouched. A comma is trailing when the next thing
/// is `}` or `]` and a value stands before it, so `[,]` and `,,` stay
/// the errors they are. `None` is a block comment that never ends.
fn plain_json(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => out.extend(chars.next()),
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '/' if chars.peek() == Some(&'/') => {
                while chars.next_if(|c| !matches!(c, '\n' | '\r')).is_some() {}
                out.push(' ');
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut star = false;
                loop {
                    let c = chars.next()?;
                    if star && c == '/' {
                        break;
                    }
                    star = c == '*';
                }
                out.push(' ');
            }
            '}' | ']' => {
                let before = out.trim_end();
                if let Some(value) = before.strip_suffix(',')
                    && !value.trim_end().ends_with(['[', '{', ',', ':'])
                {
                    let at = value.len();
                    out.replace_range(at..at + 1, " ");
                }
                out.push(c);
            }
            _ => {
                in_string = c == '"';
                out.push(c);
            }
        }
    }
    Some(out)
}

/// A text as a JSON string, quotation marks included.
fn quoted(text: &str) -> String {
    format!("\"{}\"", escape_json_string(text))
}

/// One commented-out group of the template.
fn section(out: &mut String, note: &str, lines: Vec<(String, String)>) {
    out.push_str(&format!("\n  // {note}\n"));
    for (key, value) in lines {
        out.push_str(&format!("  // {}: {value},\n", quoted(&key)));
    }
}

/// The file a launcher writes when there is none: every setting as it is
/// now, commented out, so that it sets nothing until the user says so.
pub fn template(settings: &Settings) -> String {
    let overlay = &settings.overlay;
    let mut out = String::from(
        "// rgy settings. Comments and trailing commas are allowed.\n\
         //\n\
         // Remove the // before a line to set it. A key set here wins over the\n\
         // settings panel, which shows it as [json] and does not change it.\n\
         // rgy never rewrites this file. Restart rgxy after editing it.\n\
         //\n\
         // The values below are a copy of the settings on the day this file was\n\
         // made; later panel changes are not copied here.\n\
         {\n  \"version\": 1,\n",
    );
    let line = |key: &str, value: String| (key.to_string(), value);
    let colours = |face: &str, overrides: &[(&'static str, Rgb)]| {
        overrides
            .iter()
            .map(|(role, colour)| (format!("{face}.colour.{role}"), quoted(&colour.hex())))
            .collect::<Vec<_>>()
    };

    section(
        &mut out,
        &format!("Themes: {}.", Theme::names().join(", ")),
        vec![
            line("overlay.theme", quoted(&overlay.palette.template)),
            line("tui.theme", quoted(&settings.tui.template)),
            line("fzf.theme", quoted(&settings.fzf.palette.template)),
        ],
    );
    section(
        &mut out,
        "Window width (400-1000) and result rows (1-30, limited by the screen).",
        vec![
            line("overlay.width", overlay.width.to_string()),
            line("overlay.rows", overlay.rows.to_string()),
        ],
    );
    section(
        &mut out,
        "Window position on the work area, 0-1000: x left to right, y top to bottom.",
        vec![
            line("overlay.x", overlay.x.to_string()),
            line("overlay.y", overlay.y.to_string()),
        ],
    );
    section(
        &mut out,
        "Opacity, 64 (most transparent) to 255 (opaque).",
        vec![line("overlay.alpha", overlay.alpha.to_string())],
    );
    section(
        &mut out,
        "Shortcut: \"combo ctrl+shift+p\" or \"double ctrl 400\" (milliseconds, 100-1500).",
        vec![line("hotkey", quoted(&overlay.hotkey.spell()))],
    );
    let mut moved = colours("overlay", &overlay.palette.overrides);
    moved.extend(colours("tui", &settings.tui.overrides));
    moved.extend(colours("fzf", &settings.fzf.palette.overrides));
    section(
        &mut out,
        &format!(
            "Colours over a theme, as in \"overlay.colour.text\": \"#ffffff\" (also tui. and fzf.).\n  \
             // Roles: {}.",
            Theme::role_names().join(", ")
        ),
        moved,
    );
    let args: Vec<String> = settings.fzf.args.iter().map(|arg| quoted(arg)).collect();
    section(
        &mut out,
        "Picker arguments, one string each; this list replaces the stored one.",
        vec![line("fzf-arg", format!("[{}]", args.join(", ")))],
    );
    out.push_str("}\n");
    out
}

/// Write the template where there is no file. `false` is "there is one":
/// an existing file, whatever it holds, is never opened for writing.
pub fn create_if_absent(path: &Path, settings: &Settings) -> std::io::Result<bool> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(err) => return Err(err),
    };
    if let Err(err) = file.write_all(template(settings).as_bytes()) {
        drop(file);
        // The half file is this call's own.
        let _ = std::fs::remove_file(path);
        return Err(err);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn applied(file: &File) -> Settings {
        let mut settings = Settings::default();
        file.held.apply(&mut settings);
        settings
    }

    #[test]
    fn comments_and_trailing_commas_are_read() {
        let file = parse(
            "\u{feff}// head\r\n{ /* a\n * b **/ \"version\": 1,\n\
             \"overlay.width\": 700, // tail\n\
             \"fzf-arg\": [\"--height\", \"80%\", /* last */ ], /* end */\n}\n// after",
        );
        assert!(file.problems.is_empty(), "{:?}", file.problems);
        let settings = applied(&file);
        assert_eq!(settings.overlay.width, 700);
        assert_eq!(settings.fzf.args, ["--height", "80%"]);
    }

    #[test]
    fn strings_keep_what_looks_like_a_comment_or_a_comma() {
        let args = ["http://x", "/* no */", ",}", ",]", "q\"//", "back\\"];
        let list: Vec<String> = args
            .iter()
            .map(|arg| format!("\"{}\"", escape_json_string(arg)))
            .collect();
        let file = parse(&format!("{{\"fzf-arg\": [{}]}}", list.join(",")));
        assert!(file.problems.is_empty(), "{:?}", file.problems);
        assert_eq!(applied(&file).fzf.args, args);
    }

    #[test]
    fn an_invalid_file_sets_nothing_and_says_so() {
        for text in [
            "",
            "{",
            "[]",
            "{,}",
            "{\"overlay.width\": 700,,}",
            "{\"fzf-arg\": [,]}",
            "{\"overlay.width\": ,}",
            "{\"overlay.width\": 700} /* open",
            "{\"overlay.width\": 700 \"overlay.rows\": 3}",
            "{\"version\": 2, \"overlay.width\": 700}",
        ] {
            let file = parse(text);
            assert!(file.held.is_empty(), "{text}");
            assert_eq!(file.problems.len(), 1, "{text}: {:?}", file.problems);
        }
    }

    #[test]
    fn a_bad_key_is_reported_and_the_others_apply() {
        let file = parse(
            "{\"overlay.width\": \"700\", \"overlay.rows\": 999, \"overlay.x\": 1.5,\
             \"overlay.theme\": \"nope\", \"tui.colour.text\": \"red\",\
             \"hotkey\": \"combo space\", \"future\": {\"a\": 1}, \"overlay.y\": 10}",
        );
        assert_eq!(file.problems.len(), 7, "{:?}", file.problems);
        let keys: Vec<&str> = file.held.keys().collect();
        assert_eq!(keys, ["overlay.rows", "overlay.y"]);
        assert_eq!(applied(&file).overlay.rows, 30);
    }

    /// The template sets nothing as written, and with its lines
    /// uncommented it says exactly the settings it was made from.
    #[test]
    fn the_template_is_inert_and_complete() {
        let mut settings = Settings::default();
        settings.overlay.width = 780;
        settings.overlay.x = 25;
        settings.tui.template = "nord".to_string();
        settings
            .fzf
            .palette
            .set_by_name("text", Rgb { r: 1, g: 2, b: 3 })
            .expect("text is a role");
        settings.fzf.args = vec!["--prompt=\"//\" ".to_string()];
        let text = template(&settings);

        let inert = parse(&text);
        assert!(inert.held.is_empty() && inert.problems.is_empty(), "{text}");

        let uncommented: String = text
            .lines()
            .map(|line| match line.strip_prefix("  // \"") {
                Some(rest) => format!("  \"{rest}\n"),
                None => format!("{line}\n"),
            })
            .collect();
        let file = parse(&uncommented);
        assert!(
            file.problems.is_empty(),
            "{:?}\n{uncommented}",
            file.problems
        );
        assert_eq!(applied(&file), settings);
    }
}
