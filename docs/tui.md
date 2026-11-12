# rgxy in a terminal

`rgxy --tui` is the launcher's other face. On Windows, Linux and FreeBSD a
bare `rgxy` opens the floating window (the last two share the X11 backend);
`--tui` is an alternative there and the launcher itself on macOS (and the
fallback on any machine with no X display). Both faces use the same model,
the same daemon client and — since the `Runner` seam — the same actions.

## The actions, carried out

The terminal used to *describe* what it could not do: `>copy` printed the
path and said it owns no clipboard, `>folder` printed the folder and said it
cannot open one, `>settings` printed where the file is. It now carries the
actions out through `rgy_search_ui::Runner`, which `bins/rgxy/src/tui_actions.rs`
implements from the platform backend the window uses:

| action | key | what it does |
|---|---|---|
| open | `Ctrl+O`, `>open` | opens the selected path with the platform's default program; the session stays |
| open folder | `Ctrl+Enter`, `>folder` | opens the folder that holds it (Explorer selects the file on Windows) |
| copy path | `>copy` | puts the path on the clipboard — Win32 clipboard on Windows, OSC 52 to `/dev/tty` on unix |
| preview | `Ctrl+P`, `>preview` | shows the head of the file over the results |
| actions menu | `Ctrl+K`, `>menu` | an interactive menu of the actions the selected result offers |
| settings | `Ctrl+S`, `>settings` | an editable settings panel, written back to `settings.tsv` |
| paste | `Ctrl+V` | inserts the clipboard's text into the field; bracketed paste from the terminal works too |
| choose | `Enter` | prints the selected path to stdout and leaves, so `rgxy --tui` can still be piped onward |

`Ctrl+C` still quits — in a terminal that is what the key means. On a
terminal that started without a `Runner` (the older library entry points),
every action keeps its old, honest notice: the words say what the face
really did rather than claiming a machine it does not have.

The actions menu lists what `rgy_launcher::Action::for_kind` offers for the
selected row: an image or a folder does not get the preview, exactly as in
the window. With no result selected, `Ctrl+K` shows the command legend
instead, so the keys are always discoverable.

The settings panel has the same twelve rows as the window's panel — theme,
width, rows, position, translucency, hotkey, double-tap window, terminal
theme, picker theme, picker arguments and restore defaults. `↑`/`↓` move,
`←`/`→` and `Enter` step a value, `Esc` closes, and every step writes the
file. Change the terminal theme and the frame wears it immediately.

## Clipboard on unix

The X11 clipboard belongs to the window's display connection, which a
terminal session does not have. The terminal writes OSC 52 — the escape
sequence a terminal understands — straight to `/dev/tty`, with base 64 done
in the tree. No `xclip`, no `wl-copy`, no X connection and no helper that
can hang the interface. A terminal that does not implement OSC 52 ignores
the bytes, and the notice still names the path. Reading the clipboard back
is the terminal's own bracketed-paste job; `paste_text` answers `None` on
unix rather than pretending, and `Ctrl+V` says so.

On Windows the runner asks the Win32 clipboard directly, which needs no
window to be open, so copy and paste both work there.

## Verification

```sh
cargo test -p rgy-search-ui --lib
cargo test -p rgxy
cargo clippy --workspace --all-targets --locked -- -D warnings
```

`bins/search-ui/src/runner.rs` defines the seam and `NoRunner`;
`bins/rgxy/src/tui_actions.rs` implements it. The interactive menu, the
settings panel and the action outcomes are held by unit tests with a
recording `MockRunner`, so the parity is proven without opening a file or
writing the user's settings.
