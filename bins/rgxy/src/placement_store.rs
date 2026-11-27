// SPDX-License-Identifier: Apache-2.0

//! `state.json`: where the user dragged the window, kept per monitor.
//!
//! The file is the launcher's own, generated and strict JSON, and is not a
//! settings file: nothing here reads or writes `settings.tsv` or
//! `settings.json`. It is read once, at start, and written whole after a
//! finished drag or a reset.
//!
//! ```json
//! {"version": 1, "placements": {"<monitor key>": {"x": 5000, "y": 1800}}}
//! ```
//!
//! A file that cannot be taken leaves the configured position in effect,
//! with a sentence saying why, and the next write replaces it. So does one
//! of a version this launcher does not know.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use rgy_proto::{Json, escape_json_string, read_json};

use crate::platform::backend::placement::{ANCHOR_SCALE, Anchor, Placements};

/// The version of the object this launcher reads and writes.
const VERSION: f64 = 1.0;

/// The largest file that is read: an entry is a couple of hundred bytes.
const MAX_BYTES: u64 = 256 * 1024;

/// What was read.
#[derive(Debug, Default)]
pub struct Loaded {
    pub placements: Placements,
    /// Why something was not taken, when something was not.
    pub problem: Option<String>,
}

impl Loaded {
    fn ignored(reason: &str) -> Loaded {
        Loaded {
            placements: Placements::new(),
            problem: Some(format!("state.json was ignored: {reason}")),
        }
    }
}

/// The file the placements live in.
pub fn store_path() -> Option<PathBuf> {
    let settings = crate::settings_store::store_path()?;
    Some(settings.with_file_name("state.json"))
}

/// Read the file. One that is not there holds nothing and is no problem.
pub fn load(path: &Path) -> Loaded {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Loaded::default(),
        Err(err) => return Loaded::ignored(&err.to_string()),
    };
    let mut text = String::new();
    if let Err(err) = file.take(MAX_BYTES + 1).read_to_string(&mut text) {
        return Loaded::ignored(&err.to_string());
    }
    if text.len() as u64 > MAX_BYTES {
        return Loaded::ignored("it is larger than 256 KiB");
    }
    parse(&text)
}

/// What a text holds. An entry that cannot be taken is left out alone.
fn parse(text: &str) -> Loaded {
    let Some(Json::Obj(fields)) = read_json(text) else {
        return Loaded::ignored("it is not a JSON object");
    };
    let field = |name: &str| {
        fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    };
    if field("version").and_then(Json::as_num) != Some(VERSION) {
        return Loaded::ignored("this launcher reads \"version\": 1");
    }
    let Some(Json::Obj(entries)) = field("placements") else {
        return Loaded::ignored("it has no \"placements\" object");
    };
    let mut loaded = Loaded::default();
    for (monitor, value) in entries {
        match anchor(value) {
            Some(anchor) if !monitor.is_empty() => {
                loaded.placements.insert(monitor.clone(), anchor);
            }
            _ => {
                loaded.problem = Some("state.json: a saved position was left out".to_string());
            }
        }
    }
    loaded
}

/// One entry: two whole numbers inside the anchor's range.
fn anchor(value: &Json) -> Option<Anchor> {
    let Json::Obj(fields) = value else {
        return None;
    };
    let number = |name: &str| {
        fields
            .iter()
            .find(|(key, _)| key == name)
            .and_then(|(_, value)| value.as_num())
            .filter(|n| n.fract() == 0.0 && (0.0..=f64::from(ANCHOR_SCALE)).contains(n))
            .map(|n| n as u32)
    };
    Some(Anchor {
        x: number("x")?,
        y: number("y")?,
    })
}

/// The file's text.
fn render(placements: &Placements) -> String {
    let entries: Vec<String> = placements
        .iter()
        .map(|(monitor, anchor)| {
            format!(
                "\n    \"{}\": {{\"x\": {}, \"y\": {}}}",
                escape_json_string(monitor),
                anchor.x,
                anchor.y
            )
        })
        .collect();
    format!(
        "{{\n  \"version\": 1,\n  \"placements\": {{{}\n  }}\n}}\n",
        entries.join(",")
    )
}

/// Replace the file with these placements.
pub fn save(path: &Path, placements: &Placements) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::settings_store::write_atomic(path, &render(placements))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rgy-placement-store-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn two_monitors() -> Placements {
        Placements::from([
            (
                r"\\?\display#aaa#5&1&0&uid1#{e6f07b5f}".to_string(),
                Anchor { x: 0, y: 10_000 },
            ),
            (
                r"device:\\.\DISPLAY2".to_string(),
                Anchor { x: 7321, y: 42 },
            ),
        ])
    }

    #[test]
    fn each_monitor_keeps_its_own_place_across_a_replacement() {
        let dir = folder("round-trip");
        let path = dir.join("state.json");
        let mut placements = two_monitors();
        save(&path, &placements).expect("the first write");
        let back = load(&path);
        assert_eq!(back.placements, placements);
        assert_eq!(back.problem, None);

        placements.remove(r"device:\\.\DISPLAY2");
        save(&path, &placements).expect("the replacement");
        assert_eq!(load(&path).placements, placements);

        placements.clear();
        save(&path, &placements).expect("an empty map");
        let back = load(&path);
        assert!(back.placements.is_empty() && back.problem.is_none());

        // Nothing but the file itself is left: no temporary one, and no
        // settings file.
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["state.json"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_file_holds_nothing() {
        let dir = folder("missing");
        let loaded = load(&dir.join("state.json"));
        assert!(loaded.placements.is_empty() && loaded.problem.is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_malformed_file_is_ignored_and_then_replaced() {
        let dir = folder("malformed");
        let path = dir.join("state.json");
        for text in [
            "",
            "{",
            "[]",
            "{}",
            "{\"version\": 2, \"placements\": {}}",
            "{\"version\": 1, \"placements\": []}",
            "// no comments\n{\"version\": 1, \"placements\": {}}",
        ] {
            std::fs::write(&path, text).unwrap();
            let loaded = load(&path);
            assert!(loaded.placements.is_empty(), "{text}");
            assert!(loaded.problem.is_some(), "{text}");
        }
        std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert!(load(&path).problem.is_some(), "not UTF-8");
        std::fs::write(&path, " ".repeat(MAX_BYTES as usize + 1)).unwrap();
        assert!(load(&path).problem.is_some(), "too large");

        save(&path, &two_monitors()).expect("replaced");
        assert_eq!(load(&path).placements, two_monitors());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_bad_entry_is_left_out_alone() {
        let loaded = parse(
            "{\"version\": 1, \"placements\": {\
             \"good\": {\"x\": 1, \"y\": 2}, \
             \"far\": {\"x\": 10001, \"y\": 2}, \
             \"negative\": {\"x\": -1, \"y\": 2}, \
             \"half\": {\"x\": 1.5, \"y\": 2}, \
             \"text\": {\"x\": \"1\", \"y\": 2}, \
             \"short\": {\"x\": 1}, \
             \"\": {\"x\": 1, \"y\": 2}, \
             \"flat\": 3}}",
        );
        assert_eq!(
            loaded.placements,
            Placements::from([("good".to_string(), Anchor { x: 1, y: 2 })])
        );
        assert!(loaded.problem.is_some());
    }

    #[test]
    fn a_path_that_cannot_be_written_is_an_error() {
        let dir = folder("unwritable");
        let path = dir.join("state.json");
        std::fs::create_dir_all(&path).unwrap();
        assert!(save(&path, &two_monitors()).is_err());
        assert!(path.is_dir());
        let _ = std::fs::remove_dir_all(dir);
    }
}
