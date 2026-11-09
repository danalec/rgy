// SPDX-License-Identifier: Apache-2.0

//! The help text, in the languages the launcher speaks.
//!
//! The help is the program's own description of itself, and the user reads
//! it in the language the user reads: a help in the wrong language is not
//! a help, it is a wall.
//!
//! The text is a literal and not a template of many pieces: it is read top
//! to bottom as one thing, and the pieces would only hide where the
//! paragraph breaks are. The version is the one hole in it.
//!
//! Every line of the literal ends with a continuation. A line that does
//! not is a line the compiler keeps as it is — with the source's own
//! indentation in the middle of a paragraph — and the help then shows the
//! program how it is written instead of what it says.

use rgy_launcher::text::Lang;

/// The help text.
pub fn text(lang: Lang) -> String {
    match lang {
        Lang::English => english(),
        Lang::Portuguese => portuguese(),
    }
}

/// The help, in English.
fn english() -> String {
    format!(
        "rgxy {version} — the launcher of the rgy daemon: a floating window on Windows, Linux and FreeBSD, a terminal interface elsewhere.\n\
         \n\
         usage:\n  \
           rgxy                    # the resident launcher: the window on Windows, Linux and FreeBSD; the terminal interface on macOS\n  \
           rgxy [--tui] [--query <text>] [--limit <n>] [--endpoint <e>] [--no-hook] [--show]\n  \
                [--pick] [--fzf-binary <p>] [--fzf-arg <arg>]\n\
         \n\
         options:\n  \
           --tui              the terminal interface (an alternative on Windows, Linux and FreeBSD; the launcher on macOS)\n  \
           --query <text>     one search: prints the paths and leaves\n  \
           --limit <n>        how many results (default: 50)\n  \
           --endpoint <e>     pipe:<name> | uds:<path> | stdio\n  \
           --no-hook          does not install the global shortcut\n  \
           --show             opens the window right away\n  \
           --pick             hands the results to a picker and prints the choice\n  \
           --fzf-binary <p>   the picker to run (default: fzf or sk on the PATH)\n  \
           --fzf-arg <arg>    one argument for the picker, repeatable\n  \
           -h, --help         this text\n  \
           -V, --version      the version\n\
         \n\
         The launcher is a client: the index is in the daemon (searchd), and a\n\
         keystroke is a question sent down a pipe.\n\
         \n\
         With --pick, the results go to a picker and the choice comes out on\n\
         stdout, one path per line — the same contract as --query.\n\
         \n\
         The paths come out one per line. A path that carries a\n\
         line break does not fit in one line: it is left out, and\n\
         rgxy says on stderr how many were left out.\n\
         \n\
         The default picker is fzf or sk: only they read the list\n\
         separated by NUL. A named picker that does not read it — peco,\n\
         fz — gets the list with one path per line.\n\
         \n\
         In the bar: file: folder: doc: pic: video: audio: — filters;\n\
         >open >folder >copy >preview >menu >settings — commands; grep: —\n\
         the content of the files the name search chose. The settings\n\
         live in settings.tsv, beside the index, and >settings is the\n\
         panel that writes it.\n\
         \n\
         Both faces carry out the same actions: open a result, open its\n\
         folder, copy its path, preview its head and edit the settings.\n\
         In the terminal Ctrl+O opens, Ctrl+Enter opens the folder,\n\
         Ctrl+P previews, Ctrl+K opens the actions menu, Ctrl+S edits\n\
         the settings and Ctrl+V pastes; Enter still prints the chosen\n\
         path so it can be piped onward.",
        version = env!("CARGO_PKG_VERSION"),
    )
}

/// The help, in Portuguese.
fn portuguese() -> String {
    format!(
        "rgxy {version} — o lançador do daemon rgy: uma janela flutuante no Windows, no Linux e no FreeBSD, uma interface de terminal nas outras plataformas.\n\
         \n\
         usage:\n  \
           rgxy                    # o lançador residente: a janela no Windows, no Linux e no FreeBSD; a interface de terminal no macOS\n  \
           rgxy [--tui] [--query <texto>] [--limit <n>] [--endpoint <e>] [--no-hook] [--show]\n  \
                [--pick] [--fzf-binary <p>] [--fzf-arg <arg>]\n\
         \n\
         options:\n  \
           --tui              a interface de terminal (uma alternativa no Windows, no Linux e no FreeBSD; o lançador no macOS)\n  \
           --query <texto>    uma busca só: imprime os caminhos e sai\n  \
           --limit <n>        quantos resultados (default: 50)\n  \
           --endpoint <e>     pipe:<nome> | uds:<caminho> | stdio\n  \
           --no-hook          não instala o atalho global\n  \
           --show             abre a janela logo de início\n  \
           --pick             entrega os resultados a um picker e imprime a escolha\n  \
           --fzf-binary <p>   o picker a rodar (default: fzf ou sk no PATH)\n  \
           --fzf-arg <arg>    um argumento para o picker, repetível\n  \
           -h, --help         este texto\n  \
           -V, --version      a versão\n\
         \n\
         O lançador é um cliente: o índice está no daemon (searchd), e uma\n\
         tecla é uma pergunta enviada por um pipe.\n\
         \n\
         Com --pick, os resultados vão para um picker e a escolha sai na\n\
         stdout, um caminho por linha — o mesmo contrato de --query.\n\
         \n\
         Os caminhos saem um por linha. Um caminho que carregue uma\n\
         quebra de linha não cabe numa linha: ele fica de fora, e o\n\
         rgxy diz no stderr quantos ficaram.\n\
         \n\
         O picker por omissão é o fzf ou o sk: só eles leem a lista\n\
         separada por NUL. Um picker nomeado que não a leia — o peco, o\n\
         fz — recebe a lista com um caminho por linha.\n\
         \n\
         Na barra: file: folder: doc: pic: video: audio: — filtros;\n\
         >open >folder >copy >preview >menu >settings — comandos; grep: —\n\
         o conteúdo dos ficheiros que a busca de nomes escolheu. As\n\
         configurações vivem em settings.tsv, ao lado do índice, e\n\
         >settings é o painel que o escreve.\n\
         \n\
         As duas faces executam as mesmas ações: abrir um resultado, abrir\n\
         a sua pasta, copiar o caminho, pré-visualizar o início e editar\n\
         as configurações. No terminal Ctrl+O abre, Ctrl+Enter abre a\n\
         pasta, Ctrl+P pré-visualiza, Ctrl+K abre o menu de ações, Ctrl+S\n\
         edita as configurações e Ctrl+V cola; Enter continua a imprimir\n\
         o caminho escolhido para poder ser encaminhado.",
        version = env!("CARGO_PKG_VERSION"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A paragraph is a paragraph.
    ///
    /// A source line that does not end with a continuation ends the
    /// paragraph with a raw newline and the source's own indentation, and
    /// the help then shows the program how it is written instead of what it
    /// says. The paragraph is one line in the output, in both languages.
    #[test]
    fn a_paragraph_is_one_line_in_the_output() {
        let english = english();
        let portuguese = portuguese();
        for (lang, text) in [(Lang::English, &english), (Lang::Portuguese, &portuguese)] {
            for line in text.lines() {
                assert!(
                    !line.starts_with("         "),
                    "the help carries the source's indentation: {line:?} ({lang:?})"
                );
            }
        }
        assert!(
            english.contains(
                "The paths come out one per line. A path that carries a\n\
                 line break does not fit in one line: it is left out, and\n\
                 rgxy says on stderr how many were left out."
            ),
            "the paragraph is broken: {english}"
        );
        assert!(
            portuguese.contains(
                "Os caminhos saem um por linha. Um caminho que carregue uma\n\
                 quebra de linha não cabe numa linha: ele fica de fora, e o\n\
                 rgxy diz no stderr quantos ficaram."
            ),
            "the paragraph is broken: {portuguese}"
        );
    }

    /// The help says what a bare `rgxy` does.
    ///
    /// The usage text is the program's own description of itself, and a
    /// line that does not say that the launcher is the launcher is a line
    /// that leaves the reader guessing.
    #[test]
    fn the_help_says_that_a_bare_rgxy_is_the_launcher() {
        let english = english();
        let portuguese = portuguese();
        assert!(english.contains("the resident launcher"), "{english}");
        assert!(portuguese.contains("o lançador residente"), "{portuguese}");
    }

    /// The help offers only the pickers that can read the list.
    ///
    /// The default is the picker that reads the NUL-separated list: `peco`
    /// and `fz` do not, and offering them is offering a picker that never
    /// sees the results.
    #[test]
    fn the_help_offers_only_the_pickers_that_can_read_the_list() {
        let english = english();
        let portuguese = portuguese();
        assert!(
            english.contains("default: fzf or sk on the PATH"),
            "{english}"
        );
        assert!(
            !english.contains("default: peco"),
            "the default is not a picker that cannot read the list: {english}"
        );
        assert!(
            portuguese.contains("default: fzf ou sk no PATH"),
            "{portuguese}"
        );
    }

    /// The help names the flags the command line has.
    #[test]
    fn the_help_names_the_picker_flags() {
        let english = english();
        let portuguese = portuguese();
        for needle in ["--pick", "--fzf-binary", "--fzf-arg"] {
            assert!(
                english.contains(needle),
                "the help needs `{needle}`: {english}"
            );
            assert!(
                portuguese.contains(needle),
                "the help needs `{needle}`: {portuguese}"
            );
        }
    }
}
