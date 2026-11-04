// SPDX-License-Identifier: Apache-2.0

//! The words the launcher says, and the languages they can be said in.
//!
//! Every sentence a user can read is in this module, and nowhere else:
//! the labels the menu shows, the sentences the launcher refuses with,
//! the line that says the daemon is not there, the window's placeholder.
//! The code is written in English and the **product** is not: the launcher
//! was built with Portuguese labels on purpose, and the words are the
//! product's, so they belong in a table where a second language is a
//! second column and not a patch over a dozen files.
//!
//! The table is built by a macro so the two languages cannot drift: a
//! word that one language has and the other does not is a word that does
//! not compile. The English column is the base — it is the language the
//! code is written in — and Portuguese is the translation.
//!
//! The crate is `no_std` and has no dependencies, so the table is a
//! `&'static str` per language and the language is a byte. Reading the
//! environment is the machine's business and happens in the binary; this
//! module only knows how to turn a code like `pt-BR` into a language.
//!
//! A word that does not fit the sentence around it is a word with a
//! placeholder in it: `no_preview` is `"{} has no preview"`, and the
//! thing that has no preview is put in the hole by the caller.

use alloc::string::{String, ToString};

/// The languages the launcher ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// English — the base, and the language the code is written in.
    English,
    /// Portuguese.
    Portuguese,
}

impl Lang {
    /// Every language the launcher ships.
    pub const ALL: &'static [Lang] = &[Lang::English, Lang::Portuguese];

    /// The language a locale code names.
    ///
    /// `pt`, `pt-BR` and `pt_PT` are Portuguese; `en` and `en-US` are
    /// English. Anything else is English: a language the launcher does
    /// not ship is a language it reads in English, not a launcher that
    /// says nothing at all.
    pub fn from_code(code: &str) -> Lang {
        let head = code.split(['-', '_']).next().unwrap_or("");
        if head.eq_ignore_ascii_case("pt") {
            Lang::Portuguese
        } else {
            Lang::English
        }
    }

    /// The words in this language.
    pub fn text(self) -> &'static Text {
        Text::for_lang(self)
    }
}

macro_rules! vocabulary {
    ($($field:ident => $english:expr, $portuguese:expr;)*) => {
        /// Every word the launcher says.
        ///
        /// One struct, one field per word: the code asks for the word it
        /// needs by name, and the language is the table it came from.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct Text {
            $(
                #[doc = concat!("The English word is `", $english, "`.")]
                pub $field: &'static str,
            )*
        }

        impl Text {
            /// The words in English.
            pub const ENGLISH: Text = Text {
                $($field: $english,)*
            };

            /// The words in Portuguese.
            pub const PORTUGUESE: Text = Text {
                $($field: $portuguese,)*
            };

            /// The words in one language.
            pub fn for_lang(lang: Lang) -> &'static Text {
                match lang {
                    Lang::English => &Text::ENGLISH,
                    Lang::Portuguese => &Text::PORTUGUESE,
                }
            }
        }
    };
}

vocabulary! {
    // --- the menu's labels -------------------------------------------------
    open => "Open", "Abrir";
    open_folder => "Open folder", "Abrir pasta";
    copy_path => "Copy path", "Copiar caminho";
    preview => "Preview", "Pré-visualizar";
    actions => "Actions", "Ações";

    // --- the sentence the launcher refuses with -----------------------------
    no_preview => "{} has no preview", "{} não tem pré-visualização";

    // --- the names of the kinds of thing ------------------------------------
    thing_folder => "a folder", "uma pasta";
    thing_document => "a document", "um documento";
    thing_image => "an image", "uma imagem";
    thing_video => "a video", "um vídeo";
    thing_audio => "an audio file", "um áudio";
    thing_program => "a program", "um programa";
    thing_file => "a file", "um ficheiro";

    // --- the content layer's backend ----------------------------------------
    adapters => "adapters", "adaptadores";
    rgx => "rgx", "rgx";
    no_content => "no content", "sem conteúdo";

    // --- the daemon ----------------------------------------------------------
    no_daemon => "no connection to the daemon", "sem ligação ao daemon";
    daemon_slow => "the daemon did not answer in time: {e}", "o daemon não respondeu a tempo: {e}";
    daemon_bad => "the daemon answered badly: {e}", "o daemon respondeu mal: {e}";
    no_daemon_in => "no daemon in {}; {}", "sem daemon em {}; {}";
    tree_not_followed => "the tree is no longer followed by the daemon",
        "a árvore já não é seguida pelo daemon";
    tree_never_followed => "the daemon does not follow this tree: the index is a snapshot",
        "o daemon não segue esta árvore: o índice é uma fotografia";
    index_read_only => "the index is in read-only mode", "o índice está em modo somente-leitura";

    // --- the searches --------------------------------------------------------
    name_search_failed => "the name search failed", "a busca de nomes falhou";
    content_search_failed => "the content search failed", "a busca de conteúdo falhou";
    no_files_to_read => "the name half gave no result; there are no files to read",
        "a parte dos nomes deu nenhum resultado; não há ficheiros para ler";

    // --- the rgx fallback ----------------------------------------------------
    rgx_needs_a_name_scope =>
        "rgx only answers over the scope of a name search, and without the daemon there was no search at all",
        "o rgx só responde sobre o escopo de uma busca de nomes, e sem o daemon não houve nenhuma";
    content_needs_the_daemon =>
        "the content layer needs the daemon (searchd) or rgx on the PATH",
        "a camada de conteúdo precisa do daemon (searchd) ou do rgx no PATH";
    no_time_for_rgx => "there was no time left for rgx", "não sobrou tempo para o rgx";
    scope_late => "the scope became ready too late: the window is {}",
        "o escopo ficou pronto tarde demais: a janela é de {}";
    scope_not_ready_in => "the scope was not ready in {}", "o escopo não ficou pronto em {}";
    scope_not_ready => "the scope was not ready", "o escopo não ficou pronto";
    rgx_no_symbols => "rgx does not answer symbol queries", "o rgx não responde a símbolos";
    rgx_not_runnable => "rgx could not be run: {e}", "o rgx não pôde ser executado: {e}";
    rgx_no_output => "rgx returned nothing", "o rgx não devolveu nada";
    rgx_failed => "rgx failed ({status})", "o rgx falhou ({status})";
    rgx_no_answer => "rgx did not answer in {}", "o rgx não respondeu em {}";
    rgx_ended_silent => "rgx ended without answering", "o rgx terminou sem responder";
    will_use_rgx => "will use rgx", "usará o rgx";

    // --- the window ----------------------------------------------------------
    placeholder => "Search files, apps or commands…", "Buscar arquivos, apps ou comandos…";
    could_not_open => "{} could not be opened ({})", "{} não pôde ser aberto ({})";
    no_room_for_the_actions => "no room for the actions", "sem espaço para as ações";
    screen_refused_the_surface => "the screen refused the translucent surface",
        "o ecrã recusou a superfície translúcida";

    // --- the name in the header ----------------------------------------------
    //
    // The graphical window's title and the terminal's header are two
    // faces of the same launcher, and two faces that carry two names
    // are a launcher the user has to learn twice: the window says
    // "rgxy", so the terminal says it too, or the two faces disagree
    // about what they are.
    //
    // The other name belongs to the tool that keeps an index in its own
    // process, and that tool says its own.
    brand_launcher => "rgxy", "rgxy";
    brand_index => "zen-rs", "zen-rs";

    // --- the clipboard -------------------------------------------------------
    clipboard_is_busy => "the clipboard is busy", "a área de transferência está ocupada";
    clipboard_is_empty => "the clipboard has no text", "a área de transferência não tem texto";
    clipboard_not_emptied => "the clipboard could not be emptied",
        "a área de transferência não pôde ser esvaziada";
    clipboard_not_reserved => "the clipboard memory could not be reserved",
        "a memória da área de transferência não pôde ser reservada";
    clipboard_not_locked => "the clipboard memory could not be locked",
        "a memória da área de transferência não pôde ser bloqueada";
    path_not_copied => "the path could not go to the clipboard",
        "o caminho não pôde ir para a área de transferência";
    nothing_to_paste => "nothing to paste", "nada para colar";
    path_copied => "path copied", "caminho copiado";

    // --- the results ---------------------------------------------------------
    no_result => "no result", "nenhum resultado";
    no_result_selected => "no result selected", "nenhum resultado selecionado";
    could_not_open_the_result => "could not be opened", "não pôde ser aberto";

    // --- the history, the settings and the hotkey ---------------------------
    history_not_read => "the history could not be read: {err}", "o histórico não pôde ser lido: {err}";
    history_not_saved => "the history was not saved: {reason}", "o histórico não foi gravado: {reason}";
    settings_not_read => "the settings could not be read: {err}", "as configurações não puderam ser lidas: {err}";
    hotkey_not_installed => "the global shortcut was not installed: {reason}",
        "o atalho global não foi instalado: {reason}";
    hotkey_not_installed_no_permission => "the global shortcut was not installed: no permission",
        "o atalho global não foi instalado: sem permissão";
    global_shortcut => "global shortcut", "atalho global";
    history => "history", "histórico";

    // --- the preview ---------------------------------------------------------
    file_not_read => "the file could not be read: {e}", "não foi possível ler o ficheiro: {e}";
    binary_file => "binary file · {} bytes", "ficheiro binário · {} bytes";
    empty_file => "(empty file)", "(ficheiro vazio)";

    // --- the notification area -----------------------------------------------
    // The words of the icon the launcher leaves in the notification area:
    // the tip the shell shows when the pointer rests on it, and the two
    // items of the menu the right button opens.
    //
    // They are here and not in the Windows crate because the vocabulary is
    // the product's: a word a user reads is a word of the table.
    tray_tip => "rgxy — search files, apps or commands",
        "rgxy — buscar arquivos, apps ou comandos";
    tray_search => "Search", "Buscar";
    tray_quit => "Quit", "Sair";

    // --- the terminal's own words --------------------------------------------
    // The terminal is the launcher without a desktop, so it says the same
    // words in the same language.
    terminal_open => "print the path", "imprime o caminho";
    terminal_open_folder => "the folder that holds it", "a pasta que o contém";
    terminal_copy_path => "the path, to copy", "o caminho, para copiar";
    terminal_preview => "the first lines of the file", "as primeiras linhas do ficheiro";
    terminal_menu => "this list", "esta lista";
    terminal_settings => "the settings, in the file", "as configurações, no ficheiro";
    terminal_settings_file => "the settings live in {}", "as configurações vivem em {}";
    notice_actions => "actions", "ações";
    notice_unknown_command => "unknown command", "comando desconhecido";

    // The terminal's notices: what a command did, and what it could not
    // do. They are the product's words like every other sentence — a
    // terminal that says its chrome in Portuguese and its panels in
    // English is a launcher that speaks two languages at once.
    notice_nothing => "nothing to act on", "nada sobre o que agir";
    notice_folder_title => "folder", "pasta";
    notice_folder => "the terminal cannot open a folder — this is the one that holds it:",
        "o terminal não abre uma pasta — esta é a que a contém:";
    notice_copy_title => "copy", "copiar";
    notice_copy => "the terminal has no clipboard — copy the path from here:",
        "o terminal não tem área de transferência — copie o caminho daqui:";
    notice_preview_title => "preview — {}", "pré-visualização — {}";

    // The box is empty: there is no question yet, and "no matches" is the
    // answer to a question nobody asked.
    terminal_type_to_search => "type to search", "escreva para procurar";

    terminal_prose_search => "every keystroke is a whole-disk search",
        "cada tecla é uma busca a todo o disco";

    // The header's count: how many entries the index holds, and the
    // sentence for a count nobody knows.
    //
    // The second one is not decoration. Zero is a fact — "there is
    // nothing here" — and a source that cannot say how big its index is
    // must not be read as an empty one: a header that says "0" about an
    // index of a thousand entries is a wrong fact, and a wrong fact is
    // worse than a sentence that says the number is not known.
    terminal_files_and_folders => "{} files and folders", "{} ficheiros e pastas";
    terminal_count_unknown => "count not known", "contagem desconhecida";

    // --- the window's own words -------------------------------------------------
    // The words the window shows that the model does not say: the line under
    // the field, the layer the answer came from, the panel's waiting line.
    // They are here and not in the binary because the vocabulary is the
    // product's: a word that a user reads is a word of the table, and a
    // table per crate is a table a word can be missing from.
    searching => "searching…", "procurando…";
    results => "{} results", "{} resultados";
    of_query => "of \"{query}\"", "de \"{query}\"";
    layer_names => "names", "nomes";
    layer_content => "content", "conteúdo";
    reading => "reading…", "a ler…";
    worker_died => "the thread that asks the daemon died",
        "a thread que pergunta ao daemon morreu";
    rgx_ended_badly => "rgx ended badly: {e}", "o rgx terminou mal: {e}";

    // The word for the panel that has the keyboard.
    //
    // The menu is a column of actions on the right of the window, and the
    // key that opens it gives it the keyboard: the arrows move the action
    // and not the result. The line under the field says so, because a key
    // that moves the selection somewhere else and says nothing is a key
    // the user cannot tell from a frozen one.
    menu_has_the_keyboard => "the actions have the keyboard",
        "as ações têm o teclado";

    // --- the time the answers take ---------------------------------------------
    // The meter's words. The percentiles are the same in every language a
    // person reads, and the cold start is the one that is translated.
    latency_first => "first", "primeiro";
    latency_p50 => "p50", "p50";
    latency_p90 => "p90", "p90";

    // --- the commands the user types -----------------------------------------
    write_a_command => "write a command: >open, >folder, >copy, >preview, >menu, >settings",
        "escreva um comando: >open, >folder, >copy, >preview, >menu, >settings";
    unknown_command => "unknown command: >{name}", "comando desconhecido: >{name}";

    // --- the settings panel --------------------------------------------------
    settings => "Settings", "Configurações";
    settings_theme => "theme", "tema";
    settings_width => "width", "largura";
    settings_rows => "rows", "linhas";
    settings_position => "position", "posição";
    settings_alpha => "translucency", "transparência";
    settings_hotkey => "open with", "abrir com";
    settings_hotkey_window => "double-tap window", "janela do duplo-toque";
    settings_tui => "terminal theme", "tema do terminal";
    settings_fzf => "picker theme", "tema do seletor";
    settings_fzf_args => "picker arguments", "argumentos do seletor";
    settings_defaults => "restore defaults", "restaurar padrões";
    settings_not_saved => "the settings were not saved: {reason}",
        "as configurações não foram gravadas: {reason}";
    hotkey_not_armed => "the hotkey could not be armed: {reason}",
        "o atalho não pôde ser ativado: {reason}";

    // --- the command line ----------------------------------------------------
    pick_needs_a_query => "--pick needs --query: a picker filters the results of a search",
        "--pick precisa de --query: um picker filtra os resultados de uma busca";
    daemon_address_unknown =>
        "the daemon's address is unknown: neither XDG_CACHE_HOME nor HOME is set",
        "não se sabe onde o daemon escuta: nem XDG_CACHE_HOME nem HOME estão definidos";
    fzf_binary_needs_a_path => "--fzf-binary needs a path", "--fzf-binary precisa de um caminho";
    fzf_arg_needs_an_argument => "--fzf-arg needs an argument", "--fzf-arg precisa de um argumento";
    query_needs_text => "--query needs some text", "--query precisa de um texto";
    limit_needs_a_number => "--limit needs a number", "--limit precisa de um número";
    limit_is_not_a_number => "--limit needs a number: {value}", "--limit precisa de um número: {value}";
    endpoint_needs_an_address => "--endpoint needs an address", "--endpoint precisa de um endereço";
    unknown_option => "unknown option `{other}`", "opção desconhecida `{other}`";
    unknown_endpoint => "unknown endpoint: {text} (use pipe:<name>, uds:<path> or stdio)",
        "endpoint desconhecido: {text} (use pipe:<nome>, uds:<caminho> ou stdio)";
    the_write_failed => "the write failed: {err}", "a escrita falhou: {err}";
    a_picker_needs_a_terminal => "a picker needs a terminal", "um picker precisa de um terminal";
    the_results_did_not_arrive => "the results did not reach the picker: {e}",
        "os resultados não chegaram ao picker: {e}";
    no_graphical_screen => "no graphical screen; the terminal interface is the alternative",
        "sem ecrã gráfico; a interface de terminal é a alternativa";
    the_window_is_windows_only =>
        "the floating window is Windows-only for now; the terminal interface is the alternative",
        "a janela flutuante é só no Windows por agora; a interface de terminal é a alternativa";
}

/// Which program is showing the interface.
///
/// The launcher has two faces — the floating window and the terminal —
/// and the window's title carries the launcher's name, so the terminal's
/// header carries it too. The tool that keeps an index in its own
/// process is a different program, and it says its own name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Brand {
    /// The launcher: the window, and the terminal that stands in for it
    /// where there is no desktop.
    Launcher,
    /// The tool that owns an index in its own process.
    Index,
}

impl Brand {
    /// The name, in the language of the table.
    ///
    /// The two names are the same in every language — they are the names
    /// of programs, not sentences — but they come from the table anyway,
    /// because a word a user reads is a word of the table.
    pub fn word(self, text: &Text) -> &'static str {
        match self {
            Brand::Launcher => text.brand_launcher,
            Brand::Index => text.brand_index,
        }
    }
}

/// Fill the holes of a sentence, in order.
///
/// The vocabulary's positional holes are `{}`, and the caller says what
/// goes into them: the first hole takes the first thing, the second the
/// second. A hole with a name in it — `{query}` — belongs to the caller
/// that owns the name, and is filled where it is read.
pub fn fill(sentence: &str, holes: &[&str]) -> String {
    let mut out = sentence.to_string();
    for hole in holes {
        out = out.replacen("{}", hole, 1);
    }
    out
}

impl Text {
    /// The words in English — the base, and what the launcher speaks when
    /// nothing says otherwise.
    pub fn english() -> &'static Text {
        &Text::ENGLISH
    }

    /// The words in Portuguese.
    pub fn portuguese() -> &'static Text {
        &Text::PORTUGUESE
    }
}

/// The words in the language the launcher is speaking.
///
/// The launcher is a client and the window is a machine, so the words are
/// the machine's: the binary reads the environment once and hands the
/// language down, and everything below asks for the word it needs.
pub fn text(lang: Lang) -> &'static Text {
    lang.text()
}

/// The sentence the launcher says when the daemon is not there.
///
/// It is a function and not a constant because it is a word in the
/// vocabulary, and the vocabulary is per language.
pub fn no_daemon(lang: Lang) -> &'static str {
    Text::for_lang(lang).no_daemon
}

/// The name of a kind of thing, in the user's language.
///
/// It is what the refusal names: the user does not have to guess which
/// thing the launcher refused.
pub fn thing_of(kind: crate::results::IconKind, lang: Lang) -> &'static str {
    let text = Text::for_lang(lang);
    match kind {
        crate::results::IconKind::Folder => text.thing_folder,
        crate::results::IconKind::Document => text.thing_document,
        crate::results::IconKind::Image => text.thing_image,
        crate::results::IconKind::Video => text.thing_video,
        crate::results::IconKind::Audio => text.thing_audio,
        crate::results::IconKind::Application => text.thing_program,
        crate::results::IconKind::File => text.thing_file,
    }
}

/// The sentence the launcher refuses with, in the user's language.
pub fn no_preview_of(kind: crate::results::IconKind, lang: Lang) -> String {
    Text::for_lang(lang)
        .no_preview
        .replace("{}", thing_of(kind, lang))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn english_is_the_base() {
        assert_eq!(Text::for_lang(Lang::English), &Text::ENGLISH);
        assert_eq!(Text::english().open, "Open");
    }

    #[test]
    fn the_code_names_the_language() {
        assert_eq!(Lang::from_code("en"), Lang::English);
        assert_eq!(Lang::from_code("en-US"), Lang::English);
        assert_eq!(Lang::from_code("pt"), Lang::Portuguese);
        assert_eq!(Lang::from_code("pt-BR"), Lang::Portuguese);
        assert_eq!(Lang::from_code("pt_PT"), Lang::Portuguese);
        assert_eq!(Lang::from_code("PT-br"), Lang::Portuguese);
    }

    #[test]
    fn a_language_the_launcher_does_not_ship_is_english() {
        assert_eq!(Lang::from_code("de-DE"), Lang::English);
        assert_eq!(Lang::from_code(""), Lang::English);
        assert_eq!(Lang::from_code("klingon"), Lang::English);
    }

    #[test]
    fn both_languages_have_a_word_for_every_field() {
        // The macro makes a missing field a compile error; this makes an
        // empty one a test failure, because a word that is empty is a
        // blank label on the screen.
        for lang in Lang::ALL {
            let text = Text::for_lang(*lang);
            assert!(!text.open.is_empty(), "{lang:?}");
            assert!(!text.open_folder.is_empty(), "{lang:?}");
            assert!(!text.copy_path.is_empty(), "{lang:?}");
            assert!(!text.preview.is_empty(), "{lang:?}");
            assert!(!text.actions.is_empty(), "{lang:?}");
            assert!(!text.no_preview.is_empty(), "{lang:?}");
            assert!(!text.thing_folder.is_empty(), "{lang:?}");
            assert!(!text.placeholder.is_empty(), "{lang:?}");
            assert!(!text.no_daemon.is_empty(), "{lang:?}");
            assert!(!text.menu_has_the_keyboard.is_empty(), "{lang:?}");
            assert!(!text.terminal_files_and_folders.is_empty(), "{lang:?}");
            assert!(!text.terminal_count_unknown.is_empty(), "{lang:?}");
            assert!(!text.brand_launcher.is_empty(), "{lang:?}");
            assert!(!text.brand_index.is_empty(), "{lang:?}");
        }
    }

    #[test]
    fn the_refusal_names_the_thing_in_the_language() {
        let reason = no_preview_of(crate::results::IconKind::Folder, Lang::English);
        assert!(reason.contains("a folder"), "{reason}");
        assert!(reason.contains("no preview"), "{reason}");

        let reason = no_preview_of(crate::results::IconKind::Folder, Lang::Portuguese);
        assert!(reason.contains("uma pasta"), "{reason}");
        assert!(reason.contains("pré-visualização"), "{reason}");
    }

    #[test]
    fn every_language_ships_and_english_is_one_of_them() {
        assert_eq!(Lang::ALL.len(), 2);
        assert!(Lang::ALL.contains(&Lang::English));
        assert!(Lang::ALL.contains(&Lang::Portuguese));
    }

    #[test]
    fn the_two_languages_are_not_the_same_table() {
        // A table that was copied into both columns would compile and
        // would ship the wrong language to half the users.
        assert_ne!(Text::ENGLISH.open, Text::PORTUGUESE.open);
        assert_ne!(Text::ENGLISH.placeholder, Text::PORTUGUESE.placeholder);
        assert_eq!(Text::ENGLISH.rgx, Text::PORTUGUESE.rgx);
    }

    #[test]
    fn the_brand_says_the_name_of_the_program_that_shows_the_interface() {
        // The launcher has two faces — the floating window and the
        // terminal — and one name, and the window's title carries it, so
        // the terminal's header carries it too.
        assert_eq!(Brand::Launcher.word(Text::for_lang(Lang::English)), "rgxy");
        assert_eq!(Brand::Index.word(Text::for_lang(Lang::English)), "zen-rs");

        // A name is a name in every language.
        assert_eq!(
            Brand::Launcher.word(Text::for_lang(Lang::Portuguese)),
            "rgxy"
        );
        assert_eq!(
            Brand::Index.word(Text::for_lang(Lang::Portuguese)),
            "zen-rs"
        );
    }

    #[test]
    fn the_text_is_the_table_of_the_language() {
        assert_eq!(text(Lang::Portuguese).open, "Abrir");
        assert_eq!(no_daemon(Lang::Portuguese), "sem ligação ao daemon");
        assert_eq!(
            no_daemon(Lang::English).to_string(),
            "no connection to the daemon"
        );
    }
}
