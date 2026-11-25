// SPDX-License-Identifier: Apache-2.0

//! The language the launcher speaks.
//!
//! The launcher is `std`, and the environment is what says which language
//! the user reads. The answer is read **once**, at start-up, and handed
//! down: a launcher that changed its language in the middle of a sentence
//! would be a launcher that says two things at once.
//!
//! The order is the one the shells use. `RGY_LANG` is this program's own
//! name for the question and it wins; `LC_ALL` is the user's answer to
//! everything; `LANG` is the machine's default. A machine that says
//! nothing gets English — the language the code is written in, and the one
//! the launcher falls back to.

use rgy_launcher::text::Lang;

/// The language the environment asks for.
pub fn from_env() -> Lang {
    from(|name| std::env::var(name).ok())
}

/// The language the environment asks for, read through `get`.
///
/// The reader is a parameter so that the order can be tested without
/// touching the environment: the environment of a test is the environment
/// of the machine it runs on, and a test that changes it changes it for
/// every other test too.
fn from(get: impl Fn(&str) -> Option<String>) -> Lang {
    for name in ["RGY_LANG", "LC_ALL", "LANG"] {
        let Some(code) = get(name) else {
            continue;
        };
        let code = code.trim();
        // An empty variable says nothing, and a variable that says
        // nothing does not answer the question: the next one is asked.
        if !code.is_empty() {
            return Lang::from_code(code);
        }
    }
    Lang::English
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The program's own name for the question wins.
    ///
    /// The user who sets `RGY_LANG` is asking for that language and not
    /// for the one the shell happens to carry.
    #[test]
    fn the_programs_own_variable_wins() {
        let lang = from(|name| match name {
            "RGY_LANG" => Some("pt-BR".to_string()),
            "LC_ALL" => Some("en_US.UTF-8".to_string()),
            "LANG" => Some("en_US.UTF-8".to_string()),
            _ => None,
        });
        assert_eq!(lang, Lang::Portuguese);
    }

    /// `LC_ALL` is the user's answer to everything, and it wins over the
    /// machine's default.
    #[test]
    fn the_locale_all_wins_over_the_locale() {
        let lang = from(|name| match name {
            "LC_ALL" => Some("pt_PT".to_string()),
            "LANG" => Some("en_US.UTF-8".to_string()),
            _ => None,
        });
        assert_eq!(lang, Lang::Portuguese);
    }

    /// The machine's default is the last word.
    #[test]
    fn the_locale_is_the_last_word() {
        let lang = from(|name| match name {
            "LANG" => Some("pt".to_string()),
            _ => None,
        });
        assert_eq!(lang, Lang::Portuguese);
    }

    /// A machine that says nothing gets English.
    ///
    /// The launcher ships in two languages, and a launcher that says
    /// nothing at all is a launcher the user cannot read.
    #[test]
    fn a_machine_that_says_nothing_gets_english() {
        assert_eq!(from(|_| None), Lang::English);
    }

    /// A variable that is set to nothing is not an answer.
    ///
    /// An empty `RGY_LANG` is a shell that exported an empty string, and
    /// the question is still open.
    #[test]
    fn an_empty_variable_does_not_answer_the_question() {
        let lang = from(|name| match name {
            "RGY_LANG" => Some(String::new()),
            "LANG" => Some("pt".to_string()),
            _ => None,
        });
        assert_eq!(lang, Lang::Portuguese);
    }
}
