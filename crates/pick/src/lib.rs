// SPDX-License-Identifier: Apache-2.0

//! The picker: rgy's results handed to an external `fzf`.
//!
//! The picker is a separate program, found by scanning `PATH` for the
//! candidate names below — and the scan is memoised per process, because an
//! unscanned `PATH` costs one stat per candidate per call.
//!
//! Nothing here runs unless the caller asked for a picker: no probe, no
//! spawn, no terminal check. The exit status is the whole contract of a
//! search-shaped tool, so it is spelled out rather than inherited:
//!
//! | code | meaning |
//! |------|---------|
//! | 0    | the search matched and the picker returned a selection |
//! | 1    | nothing matched, or the user dismissed the picker |
//! | 2    | no picker on `PATH`, or it could not be run |
//!
//! # The separator
//!
//! A path is bytes, and on unix a byte may be a tab or a newline. A picker
//! that reads one record per line cannot be handed a path that contains one.
//! So the record separator is a decision, and it is [`Separator::Nul`] by
//! default: the NUL byte cannot appear in a path on any platform.
//!
//! The picker has the last word on the LIST. Only the pickers that know
//! `--read0` and `--print0` — `fzf` and `sk` — read a NUL-separated list,
//! and those are the ones the crate looks for when the caller named none:
//! the default separator is NUL, and a picker that speaks it carries every
//! record as it is. A picker the caller named that does not speak it is
//! given the line separator instead, because every picker reads lines,
//! and the crate translates the records on the way in. The caller writes
//! its records once, in the separator it asked for.
//!
//! One record cannot always be carried. A path with a newline in it does
//! not survive the translation into lines, and half of it is a path that
//! does not exist, so such a record is not offered at all — and the crate
//! says on stderr how many records were left out.
//!
//! The caller has the last word on the SELECTION: what the picker chose
//! is printed in the separator the caller asked for, so a caller that
//! asked for NUL reads a NUL-separated answer.
//!
//! # The ranking
//!
//! rgy scores a name with its own fzf-v1 scorer, and the picker would
//! score it again with a newer one. The caller decides: send the score
//! along and let the picker filter without reordering
//! (`--with-nth=2.. --no-sort`), and the order the user sees is the order
//! rgy ranked. A search engine's ranking is the product; a picker that
//! replaces it is a picker that throws it away.

#![no_std]

extern crate alloc;
#[cfg(any(test, feature = "std"))]
#[macro_use]
extern crate std;

// The machine — the process, the pipes, the `PATH` — is behind the `std`
// feature, and so are the imports it needs. A build without it carries
// the vocabulary and the decisions, and not one line that touches the
// machine: which picker, and what its exit status means.
//
// The gate is the feature and not `any(test, feature = "std")`: the test
// target without the feature is a target that uses neither of these, and
// an import that is there and unused is a warning in a build that is
// supposed to be clean. The gate and the uses have to agree, and the
// uses are all behind the feature.
#[cfg(feature = "std")]
use alloc::boxed::Box;
use alloc::string::String;
#[cfg(feature = "std")]
use alloc::string::ToString;
use alloc::vec::Vec;

/// The error the picker reports: a sentence a person reads.
pub type Error = String;

/// The result of an operation that can fail with a sentence.
pub type Result<T> = core::result::Result<T, Error>;

/// The pickers that read and write NUL-separated records.
///
/// These are the ones the crate looks for when the caller named none:
/// the default separator is NUL, and a picker that speaks it needs no
/// translation — and a path with a newline in it does not survive one.
/// `peco` and `fz` do not know `--read0` and `--print0`, so a caller that
/// names one of them gets the list translated into lines.
pub const NUL_CAPABLE: &[&str] = &["fzf", "sk"];

/// The pickers the crate looks for when the caller named none.
///
/// The same two, because the default search looks for a picker that can
/// answer the NUL-separated list the crate writes. First hit on `PATH`
/// wins.
pub const CANDIDATES: &[&str] = NUL_CAPABLE;

/// `ETXTBSY`, the errno a platform says "this file is open for writing".
///
/// It is 26 on Linux, on macOS and on the BSDs, so the number is the same
/// on every platform this crate runs a picker on. Windows has no such a
/// refusal — and 26 there is another error entirely — so the wait that
/// uses it is a unix one.
///
/// The gate is the `std` feature as well: the number belongs to the wait,
/// and the wait belongs to the machine.
#[cfg(feature = "std")]
#[cfg(unix)]
const ETXTBSY: i32 = 26;

/// How the picker reads its list and writes its selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Separator {
    /// One record per line.
    Line,
    /// One record per NUL byte.
    Nul,
}

impl Separator {
    /// The flags a picker needs to read and write this separator.
    ///
    /// Empty for a line separator: that is what every picker does already.
    pub fn args(&self) -> &'static [&'static str] {
        match self {
            Separator::Line => &[],
            Separator::Nul => &["--read0", "--print0"],
        }
    }

    /// The byte that separates the records.
    pub fn byte(&self) -> u8 {
        match self {
            Separator::Line => b'\n',
            Separator::Nul => 0,
        }
    }

    /// Split what the picker wrote into records.
    pub fn split<'a>(&self, bytes: &'a [u8]) -> Vec<&'a [u8]> {
        let mut out = Vec::new();
        match self {
            Separator::Line => {
                for part in bytes.split(|b| *b == b'\n') {
                    if !part.is_empty() {
                        out.push(part);
                    }
                }
            }
            Separator::Nul => {
                for part in bytes.split(|b| *b == 0) {
                    if !part.is_empty() {
                        out.push(part);
                    }
                }
            }
        }
        out
    }
}

/// The separator a picker reads and writes.
///
/// The caller asks for one, and the picker has the last word on the list:
/// only the pickers in [`NUL_CAPABLE`] know `--read0` and `--print0`, so a
/// picker outside it gets the line separator — one record per line is what
/// every picker reads, and a path with a newline in it is the rare case
/// the crate will not offer to a picker that cannot carry it. The name is
/// the only thing the crate knows the picker by, so a path to `fzf.exe` is
/// `fzf` and is treated as `fzf`.
pub fn separator_for(binary: &str, requested: Separator) -> Separator {
    match requested {
        Separator::Line => Separator::Line,
        Separator::Nul => {
            let stem = stem(binary);
            if NUL_CAPABLE
                .iter()
                .any(|name| stem.eq_ignore_ascii_case(name))
            {
                Separator::Nul
            } else {
                Separator::Line
            }
        }
    }
}

/// The name of the picker, without the directory and without the extension.
fn stem(binary: &str) -> &str {
    let name = binary.rsplit(['/', '\\']).next().unwrap_or(binary);
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    }
}

/// Whether the picker keeps the order it was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// The list stays in the order the search produced it.
    ///
    /// rgy's ranking is the product: a picker that re-scores the list with
    /// a newer algorithm replaces it with an answer nobody asked for. The
    /// flag that says so is `--no-sort`, and only the pickers that know it
    /// get it — a picker that does not understand the flag would refuse to
    /// start, which is loud but useless.
    Keep,
    /// The picker sorts the list its own way.
    Sort,
}

/// The pickers that understand `--no-sort`.
///
/// `fzf` and `sk` both do; `peco` does not, and handing it the flag is a
/// picker that refuses to run.
pub const ORDER_AWARE: &[&str] = &["fzf", "sk"];

/// How the picker is invoked.
pub struct Request<'a> {
    /// argv appended after the binary and after the crate's own flags,
    /// unparsed — one flag per element, so nothing the user wrote is
    /// re-split by a shell.
    pub args: &'a [String],
    /// An explicit picker path; `PATH` is scanned when this is `None`.
    pub binary: Option<&'a str>,
    /// The separator the records are written with.
    ///
    /// The picker may read another one: [`separator_for`] has the last
    /// word on the list, and the crate translates the records on the way
    /// in. A record the translation cannot carry is not offered, and the
    /// crate says how many were left out.
    pub separator: Separator,
    /// Whether the picker keeps the order it was given.
    pub order: Order,
}

impl<'a> Request<'a> {
    /// A request with the line separator and no picker named.
    pub fn new(args: &'a [String]) -> Request<'a> {
        Request {
            args,
            binary: None,
            separator: Separator::Line,
            order: Order::Sort,
        }
    }
}

/// The search to offer, run on its own thread so its bytes reach the picker
/// as they are produced rather than after the whole walk finishes.
#[cfg(feature = "std")]
pub type Search =
    Box<dyn FnOnce(&mut (dyn std::io::Write + Send + 'static)) -> Result<bool> + Send + 'static>;

/// Maps a finished picker to the exit status.
///
/// The picker exits 130 when the user dismisses it and 1 when its list is
/// empty; both are "the user got nothing", which is what 1 already means
/// to every other caller. Anything else is a broken picker, not a search
/// with no matches, and it says so.
pub fn status(code: i32, no_selection: bool) -> Result<i32> {
    match (code, no_selection) {
        (0, false) => Ok(0),
        (0 | 1 | 130, _) => Ok(1),
        c => Err(alloc::format!("the picker exited with {}", c.0)),
    }
}

/// The picker that will run, and the separator it will speak.
#[cfg(feature = "std")]
struct Plan {
    binary: String,
    separator: Separator,
}

/// The picker that will run, and the separator it will speak.
///
/// The separator follows the picker: one that cannot read a NUL-separated
/// list gets the line separator, and the crate translates the records on
/// the way in.
#[cfg(feature = "std")]
fn plan(req: &Request<'_>) -> Result<Plan> {
    let binary = match req.binary {
        Some(p) => p.to_string(),
        None => locate().ok_or_else(|| {
            alloc::format!(
                "the picker needs one of {} on PATH (install one, or point --fzf-binary at another picker)",
                CANDIDATES.join(", ")
            )
        })?,
    };
    Ok(Plan {
        separator: separator_for(&binary, req.separator),
        binary,
    })
}

/// Runs `search` with its output on the picker's stdin and yields what the
/// picker returned, plus the process exit status.
///
/// The records come back rather than going to stdout, because the caller
/// knows what a record means: a path for a search, a line for a grep, and
/// only the caller can strip a column it added.
#[cfg(feature = "std")]
pub fn pick_records(search: Search, req: &Request<'_>) -> Result<(i32, Vec<Vec<u8>>)> {
    let plan = plan(req)?;
    run(search, req, &plan)
}

/// Starts the picker, and waits for the platform to let it run.
///
/// A program that is open for writing cannot be executed: the kernel
/// refuses with `ETXTBSY`, and a picker that somebody is writing — a
/// script that is being edited, a file an installer is still copying —
/// is a picker that is there. The refusal is the platform's timing and
/// not the picker's absence, so the start is tried again for a moment:
/// a caller that reads "the picker cannot run" about a picker that is
/// being written is told a falsehood.
///
/// The window is short on purpose: the write that holds the file is
/// released as soon as it is done, and a picker that stays busy for
/// longer than this is busy for a reason the caller has to know about.
#[cfg(feature = "std")]
#[cfg(unix)]
fn spawn(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(e) if e.raw_os_error() == Some(ETXTBSY) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Starts the picker.
///
/// Windows has no refusal of this kind: a program that is open for
/// writing is run all the same.
#[cfg(feature = "std")]
#[cfg(not(unix))]
fn spawn(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    command.spawn()
}

/// Runs the picker and reads back what it chose.
#[cfg(feature = "std")]
fn run(search: Search, req: &Request<'_>, plan: &Plan) -> Result<(i32, Vec<Vec<u8>>)> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;

    let mut command = Command::new(&plan.binary);
    command
        .args(plan.separator.args())
        .args(order_args(req.order, &plan.binary))
        .args(req.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // The picker draws on the terminal it opens for itself; its own
        // complaints belong on our stderr, not folded into a pipe.
        .stderr(Stdio::inherit());
    let mut child = spawn(&mut command)
        .map_err(|e| alloc::format!("the picker cannot run {}: {e}", plan.binary))?;

    let stdin = child.stdin.take().expect("stdin was piped");
    let (tx, rx) = mpsc::channel();
    // The records go in the separator the caller wrote them in, and come
    // out in the one the picker reads.
    let requested = req.separator;
    let separator = plan.separator;
    let writer = std::thread::spawn(move || {
        let mut out = Reflow::new(stdin, requested, separator);
        let result = search(&mut out);
        // The picker's list is only final at EOF: the last record may not
        // have a separator yet.
        let finished = if result.is_ok() { out.finish() } else { Ok(()) };
        let dropped = out.dropped();
        drop(out);
        let _ = tx.send((result, finished, dropped));
    });

    let mut selected: Vec<u8> = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        // A picker that dies mid-read must not take the search down with it.
        let _ = stdout.read_to_end(&mut selected);
    }
    let code = wait(&mut child);
    let (searched, finished, dropped) = rx.recv().unwrap_or((Ok(false), Ok(()), 0));
    let joined = writer.join();
    // A panic in the caller's search is not a search that found nothing:
    // the thread died, and the answer it was going to give is not there.
    // The exit status of a search-shaped tool is the whole contract — a
    // search that matched nothing is one, and a search that fell over is
    // two — so the panic is an error and says what it was.
    if let Err(payload) = joined {
        return Err(alloc::format!(
            "the search panicked: {}",
            panic_message(payload)
        ));
    }

    searched?;
    finished.map_err(|e| alloc::format!("the picker stopped reading the list: {e}"))?;
    if dropped > 0 {
        // The user has to know that a result is missing: the record was
        // left out because the picker could not have carried it whole.
        let _ = writeln!(std::io::stderr(), "{}", uncarryable(dropped, separator));
    }
    let records: Vec<Vec<u8>> = separator
        .split(&selected)
        .into_iter()
        .map(|r| r.to_vec())
        .collect();
    Ok((status(code, records.is_empty())?, records))
}

/// The caller's records, in the separator the picker reads.
///
/// The caller writes its records separated by the separator it asked for;
/// the picker reads them separated by the one it can speak. When the two
/// differ the crate translates, and a record that holds the byte the
/// picker separates on is not offered at all: half of it is a path that
/// does not exist. The crate counts what it left out, so the user can be
/// told.
///
/// When the two separators are the same there is nothing to translate,
/// and the rule still holds: the record is checked before it is handed
/// over, so a record the picker cannot read whole is left out and
/// counted whichever separator the caller asked for.
#[cfg(feature = "std")]
struct Reflow<W> {
    inner: W,
    from: Separator,
    to: Separator,
    /// The record being read, held until it is known to be carryable.
    record: Vec<u8>,
    /// Whether the record being read already holds the byte the picker
    /// separates on.
    poisoned: bool,
    /// How many records could not be carried.
    dropped: usize,
}

#[cfg(feature = "std")]
impl<W: std::io::Write> Reflow<W> {
    fn new(inner: W, from: Separator, to: Separator) -> Reflow<W> {
        Reflow {
            inner,
            from,
            to,
            record: Vec::new(),
            poisoned: false,
            dropped: 0,
        }
    }

    /// The record ends here: it is offered whole, or not at all.
    fn end_record(&mut self) -> std::io::Result<()> {
        if self.poisoned || !self.carryable() {
            self.dropped += 1;
        } else {
            self.inner.write_all(&self.record)?;
            self.inner.write_all(&[self.to.byte()])?;
        }
        self.record.clear();
        self.poisoned = false;
        Ok(())
    }

    /// The stream ends: a record the caller did not end with a separator
    /// is still a record, and it is offered whole or not at all.
    fn finish(&mut self) -> std::io::Result<()> {
        if self.poisoned || !self.carryable() {
            self.dropped += 1;
        } else if !self.record.is_empty() {
            self.inner.write_all(&self.record)?;
        }
        self.record.clear();
        self.poisoned = false;
        Ok(())
    }

    /// Whether the picker can read this record whole.
    ///
    /// A record that carries the byte the picker separates on is one the
    /// picker cannot read whole, and half of a path is a path that does
    /// not exist. A line reader strips the carriage return that ends a
    /// line too, so a record that carries one is not a line either.
    fn carryable(&self) -> bool {
        carryable_in(self.to, &self.record)
    }

    /// How many records the picker could not be handed.
    fn dropped(&self) -> usize {
        self.dropped
    }
}

#[cfg(feature = "std")]
impl<W: std::io::Write> std::io::Write for Reflow<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // A record never carries the byte it is separated by, so the
        // separator is the only byte that changes — and a record that
        // carries the byte the PICKER separates on is one the picker
        // cannot read whole.
        //
        // The two separators are often the same — a caller that asked
        // for the picker's own separator needs no translation — and the
        // rule holds then too: the record is read, checked and handed
        // over on the same path, so a record the picker cannot read is
        // left out and counted whichever separator the caller asked
        // for.
        let from = self.from.byte();
        let to = self.to.byte();
        for &byte in buf {
            if byte == from {
                self.end_record()?;
            } else if byte == to {
                self.poisoned = true;
                // The record is lost already: there is no point in holding
                // on to the bytes of a path that cannot be offered.
                self.record.clear();
            } else if !self.poisoned {
                self.record.push(byte);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// The sentence a panic in the caller's search gets.
///
/// The panic's own words are kept: they say what went wrong, and a
/// sentence that says only that something panicked says nothing a
/// reader can act on.
#[cfg(feature = "std")]
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(text) => *text,
        Err(payload) => match payload.downcast::<&str>() {
            Ok(text) => (*text).to_string(),
            Err(_) => "the reason was not a string".to_string(),
        },
    }
}

/// The warning for the records the picker could not carry.
///
/// The user has to know a result is missing: a record the picker cannot
/// read whole is not offered, and half of it is a path that does not
/// exist.
#[cfg(feature = "std")]
fn uncarryable(dropped: usize, to: Separator) -> String {
    let (byte, reads) = match to {
        Separator::Line => ("a newline", "one record per line"),
        Separator::Nul => ("a NUL byte", "one record per NUL byte"),
    };
    if dropped == 1 {
        alloc::format!(
            "1 record was not offered to the picker: it carries {byte}, and the picker reads {reads}"
        )
    } else {
        alloc::format!(
            "{dropped} records were not offered to the picker: they carry {byte}, and the picker reads {reads}"
        )
    }
}

/// Whether the record can be one line.
///
/// The line is a separator a path can carry — on unix a name may hold a
/// newline — and half of a path is a path that does not exist. So a
/// record that carries a line break is **not carried**: it is offered
/// whole, or not at all.
///
/// The carriage return is in the rule for the same reason the newline
/// is: a reader that splits on the newline strips the carriage return
/// that ends the line, and a path that ends in one comes back without
/// it.
pub fn carries_line(record: &[u8]) -> bool {
    !record.contains(&b'\n') && !record.contains(&b'\r')
}

/// Whether a record can be handed over whole, in the separator it will be
/// written with.
///
/// A record that carries the byte the reader separates on is one the reader
/// cannot read whole, and half of a path is a path that does not exist. A
/// line reader strips the carriage return that ends a line too, so a
/// record that carries one is not a line either.
///
/// The rule is one rule for every separator: the list the picker is handed
/// and the selection it hands back are read by the same reader, and a
/// record that is left out of one and written in the other is a record the
/// caller gets back broken.
///
/// The rule is public, and not behind the `std` feature: it is a rule
/// about bytes, and the crate that carries it is `no_std`. A rule that
/// only existed with an operating system under it would be a rule the
/// embedded caller could not ask about.
pub fn carryable_in(separator: Separator, record: &[u8]) -> bool {
    match separator {
        Separator::Line => carries_line(record),
        Separator::Nul => !record.contains(&0),
    }
}

/// The warning for the paths that could not be carried as lines.
///
/// A path that is missing from the list is a path the user has to be
/// told about: the line is one path, and a path that carries a line
/// break is not one line.
pub fn uncarryable_paths(dropped: usize) -> alloc::string::String {
    if dropped == 1 {
        alloc::string::String::from(
            "1 path was left out: it carries a line break, and the output is one path per line",
        )
    } else {
        alloc::format!(
            "{dropped} paths were left out: they carry a line break, and the output is one path per line"
        )
    }
}

/// Runs `search` with its output on the picker's stdin, prints what the
/// picker returns, and yields the process exit status.
///
/// The selection is printed in the separator the caller asked for — the
/// same one the records were written in — so a caller that asked for NUL
/// reads a NUL-separated answer, whatever separator the picker speaks.
#[cfg(feature = "std")]
pub fn pick(search: Search, req: &Request<'_>) -> Result<i32> {
    let plan = plan(req)?;
    let (code, records) = run(search, req, &plan)?;
    if code == 0 && !records.is_empty() {
        let mut out = std::io::stdout();
        print_selection(&records, req, &mut out)
            .map_err(|e| alloc::format!("the selection could not be printed: {e}"))?;
    }
    Ok(code)
}

/// The selection, in the separator the caller asked for.
///
/// The picker speaks the separator it knows; the caller reads the one it
/// asked for. The picker has the last word on the list, and the caller
/// has the last word on what comes back.
///
/// A selection that is printed as lines has to be one line: a record
/// that carries a line break would be two lines for whoever reads it
/// back, and half of a path is a path that does not exist. Such a
/// record is left out, and the count says how many were left out.
///
/// The rule is the separator's own: a record that carries the byte the
/// reader separates on is one the reader cannot read whole, whichever
/// separator that is. The list the picker was handed was filtered by the
/// same rule, and a selection that is printed without it would hand back
/// a record the caller cannot read.
#[cfg(feature = "std")]
fn print_selection<W: std::io::Write>(
    records: &[Vec<u8>],
    req: &Request<'_>,
    out: &mut W,
) -> std::io::Result<()> {
    let mut dropped = 0;
    for record in records {
        if !carryable_in(req.separator, record) {
            dropped += 1;
            continue;
        }
        out.write_all(record)?;
        out.write_all(&[req.separator.byte()])?;
    }
    out.flush()?;
    if dropped > 0 {
        eprintln!("{}", uncarryable_paths(dropped));
    }
    Ok(())
}

/// The flags that carry the order, when the picker understands them.
#[cfg(feature = "std")]
fn order_args(order: Order, binary: &str) -> &'static [&'static str] {
    match order {
        Order::Sort => &[],
        Order::Keep => {
            let stem = stem(binary);
            if ORDER_AWARE
                .iter()
                .any(|name| stem.eq_ignore_ascii_case(name))
            {
                &["--no-sort"]
            } else {
                &[]
            }
        }
    }
}

/// Reaps the picker. A child that died from a signal has no exit code of its
/// own; it is reported as 1, which reads as "nothing was selected".
#[cfg(feature = "std")]
fn wait(child: &mut std::process::Child) -> i32 {
    match child.wait() {
        Ok(s) => s.code().unwrap_or(1),
        Err(_) => 1,
    }
}

/// The picker on `PATH`, memoised per process.
#[cfg(feature = "std")]
fn locate() -> Option<String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static FOUND: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    let mut memo = FOUND
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    for name in CANDIDATES {
        let hit = memo
            .entry((*name).to_string())
            .or_insert_with(|| scan(name));
        if let Some(path) = hit.as_ref() {
            return Some(path.clone());
        }
    }
    None
}

/// Whether the picker that would run takes `fzf`'s colour flags.
///
/// `--color` is `fzf`'s own vocabulary: `sk` spells its colours another
/// way and `peco` has none, and a flag a picker does not understand is
/// a picker that refuses to run. The question is asked about the picker
/// the request would run — the one the caller named, or the first one
/// the `PATH` has — because the answer decides whether the caller's
/// colour flags are handed over at all.
#[cfg(feature = "std")]
pub fn takes_colour_flags(binary: Option<&str>) -> bool {
    match binary {
        Some(named) => stem(named).eq_ignore_ascii_case("fzf"),
        None => locate()
            .map(|found| stem(&found).eq_ignore_ascii_case("fzf"))
            .unwrap_or(false),
    }
}

/// Look for one picker on `PATH`.
#[cfg(feature = "std")]
fn scan(binary: &str) -> Option<String> {
    use std::path::Path;
    if binary.contains('/') || binary.contains('\\') {
        return Path::new(binary).is_file().then(|| binary.to_string());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "bat", "cmd"] {
                let candidate = candidate.with_extension(ext);
                if candidate.is_file() {
                    return Some(candidate.to_string_lossy().into_owned());
                }
            }
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "std")]
    use std::io::Write;

    #[test]
    fn a_selection_is_zero_and_a_dismissal_is_one() {
        assert_eq!(status(0, false).unwrap(), 0);
        assert_eq!(status(0, true).unwrap(), 1);
        // The picker's own "no match" and "user pressed ESC" statuses.
        assert_eq!(status(1, true).unwrap(), 1);
        assert_eq!(status(130, true).unwrap(), 1);
        // Anything else is a broken picker, not a search with no matches.
        assert!(status(2, true).is_err());
        assert!(status(127, true).is_err());
    }

    #[test]
    fn the_default_search_looks_for_the_pickers_that_read_nul() {
        // The default search looks for a picker that can answer the
        // NUL-separated list: `peco` and `fz` cannot, and offering them
        // is offering a picker that never sees the results.
        assert_eq!(CANDIDATES, NUL_CAPABLE);
        assert!(CANDIDATES.contains(&"fzf"));
        assert!(CANDIDATES.contains(&"sk"));
        assert!(!CANDIDATES.contains(&"peco"));
        assert!(!CANDIDATES.contains(&"fz"));
    }

    #[test]
    fn the_separator_follows_the_picker() {
        // fzf and sk read a NUL-separated list, wherever they are.
        assert_eq!(separator_for("fzf", Separator::Nul), Separator::Nul);
        assert_eq!(separator_for("sk", Separator::Nul), Separator::Nul);
        assert_eq!(
            separator_for("/usr/bin/fzf", Separator::Nul),
            Separator::Nul
        );
        assert_eq!(
            separator_for("C:/bin/sk.exe", Separator::Nul),
            Separator::Nul
        );
        // A picker that does not know `--read0` gets the lines.
        assert_eq!(separator_for("peco", Separator::Nul), Separator::Line);
        assert_eq!(
            separator_for("/usr/local/bin/fz", Separator::Nul),
            Separator::Line
        );
        // A picker rgy does not know by name gets the lines too: every
        // picker reads lines, and only fzf and sk are known to read NUL.
        assert_eq!(
            separator_for("/opt/my-picker", Separator::Nul),
            Separator::Line
        );
        // A line list is a line list, whoever reads it.
        assert_eq!(separator_for("fzf", Separator::Line), Separator::Line);
        assert_eq!(separator_for("peco", Separator::Line), Separator::Line);
    }

    #[test]
    fn a_nul_record_keeps_a_path_that_has_a_tab_in_it() {
        let raw = b"src/a\tb.txt\0src/c.txt\0";
        let records = Separator::Nul.split(raw);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0], b"src/a\tb.txt");
        assert_eq!(records[1], b"src/c.txt");
    }

    #[test]
    fn a_line_record_splits_on_the_newline() {
        let raw = b"src/a.txt\nsrc/b.txt\n";
        let records = Separator::Line.split(raw);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0], b"src/a.txt");
    }

    #[test]
    fn an_empty_selection_is_no_records() {
        assert!(Separator::Nul.split(b"").is_empty());
        assert!(Separator::Nul.split(b"\0\0").is_empty());
    }

    #[cfg(feature = "std")]
    #[test]
    fn an_absent_name_is_not_located() {
        assert!(scan("rgy-no-such-picker").is_none());
        assert!(scan("rgy-no-such-picker/sub").is_none());
    }

    #[cfg(feature = "std")]
    #[test]
    fn only_a_picker_that_knows_the_flag_is_told_to_keep_the_order() {
        // fzf and sk both know `--no-sort`; handing it to a picker that does
        // not is a picker that refuses to start.
        assert_eq!(order_args(Order::Keep, "/usr/bin/fzf"), &["--no-sort"]);
        assert_eq!(order_args(Order::Keep, "C:/bin/sk.exe"), &["--no-sort"]);
        assert!(order_args(Order::Keep, "/usr/bin/peco").is_empty());
        assert!(order_args(Order::Sort, "/usr/bin/fzf").is_empty());
    }

    /// Only `fzf` is asked about the colours.
    ///
    /// `--color` is fzf's own vocabulary: sk spells its colours another
    /// way and peco has none, and a flag a picker does not understand
    /// is a picker that refuses to run. The question is about the
    /// picker the caller named, whatever its directory and extension.
    #[cfg(feature = "std")]
    #[test]
    fn only_fzf_is_asked_about_the_colours() {
        assert!(takes_colour_flags(Some("/usr/bin/fzf")));
        assert!(takes_colour_flags(Some("C:/bin/fzf.exe")));
        assert!(!takes_colour_flags(Some("/usr/bin/sk")));
        assert!(!takes_colour_flags(Some("C:/bin/peco.exe")));
        assert!(!takes_colour_flags(Some("fzf-like")));
    }

    #[cfg(feature = "std")]
    #[test]
    fn a_picker_that_reads_lines_gets_the_lines() {
        // The records go in the separator the caller wrote them in, and
        // come out in the one the picker reads.
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut out = Reflow::new(&mut buf, Separator::Nul, Separator::Line);
            std::io::Write::write_all(&mut out, b"src/a.txt\0src/b.txt\0").unwrap();
        }
        assert_eq!(buf, b"src/a.txt\nsrc/b.txt\n");
    }

    #[cfg(feature = "std")]
    #[test]
    fn a_record_the_picker_cannot_carry_is_not_offered_at_all() {
        // A path with a newline in it cannot be handed to a picker that
        // reads lines, and half of it is a path that does not exist.
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut out = Reflow::new(&mut buf, Separator::Nul, Separator::Line);
            out.write_all(b"src/a.txt\0src/has\nnewline.txt\0src/c.txt\0")
                .unwrap();
        }
        // What the picker reads is two whole records: the one that cannot
        // be carried is not there, and it is not there in halves either.
        assert_eq!(buf, b"src/a.txt\nsrc/c.txt\n");
    }

    /// A record that carries a line break is not one line.
    ///
    /// The line is a separator a path can carry, and half of a path is a
    /// path that does not exist: the record is not carried, and the
    /// caller is told how many were left out.
    #[test]
    fn a_record_that_carries_a_line_break_is_not_one_line() {
        assert!(carries_line(b"src/a.txt"), "a plain path is one line");
        assert!(carries_line(b"src/a b.txt"), "a space is not a break");
        assert!(!carries_line(b"src/has\nnewline.txt"), "the newline");
        assert!(!carries_line(b"src/has\rreturn.txt"), "the carriage return");
        // And the warning says what is missing and why.
        let warning = uncarryable_paths(2);
        assert!(warning.contains("2 paths"), "{warning}");
        assert!(warning.contains("line break"), "{warning}");
        assert!(warning.contains("one path per line"), "{warning}");
    }

    #[cfg(feature = "std")]
    #[test]
    fn the_records_the_picker_cannot_carry_are_counted_and_named() {
        let mut buf: Vec<u8> = Vec::new();
        let dropped = {
            let mut out = Reflow::new(&mut buf, Separator::Nul, Separator::Line);
            out.write_all(b"src/a.txt\0src/has\nnewline.txt\0").unwrap();
            out.finish().unwrap();
            out.dropped()
        };
        assert_eq!(dropped, 1, "the record that cannot be carried is counted");
        // The user has to be told what is missing, and why.
        let warning = uncarryable(dropped, Separator::Line);
        assert!(warning.contains("1 record"), "{warning}");
        assert!(warning.contains("a newline"), "{warning}");
        assert!(warning.contains("one record per line"), "{warning}");
    }

    /// A record the picker cannot read whole is not offered at all — and
    /// the rule holds when the separator is the picker's own.
    ///
    /// A carriage return is in the rule for the same reason the newline
    /// is: a reader that splits on the newline strips the carriage
    /// return that ends the line, and a path that ends in one comes back
    /// without it. The caller asked for the lines and the picker reads
    /// the lines, and there is nothing to translate between them — and
    /// the record that cannot be carried is still not offered.
    #[cfg(feature = "std")]
    #[test]
    fn a_record_the_line_picker_cannot_carry_is_not_offered_at_all() {
        let mut buf: Vec<u8> = Vec::new();
        let dropped = {
            let mut out = Reflow::new(&mut buf, Separator::Line, Separator::Line);
            out.write_all(b"src/a.txt\nsrc/has\rreturn.txt\nsrc/c.txt\n")
                .unwrap();
            out.finish().unwrap();
            out.dropped()
        };
        assert_eq!(dropped, 1, "the record the picker cannot carry is counted");
        assert_eq!(
            buf, b"src/a.txt\nsrc/c.txt\n",
            "and it is not offered, not even in halves"
        );
    }

    #[cfg(feature = "std")]
    #[test]
    fn the_selection_is_printed_in_the_separator_the_caller_asked_for() {
        let args: Vec<String> = Vec::new();
        let req = Request {
            args: &args,
            binary: None,
            separator: Separator::Nul,
            order: Order::Sort,
        };
        let records = vec![b"src/a.txt".to_vec()];
        let mut buf: Vec<u8> = Vec::new();
        print_selection(&records, &req, &mut buf).unwrap();
        assert_eq!(buf, b"src/a.txt\0", "the caller asked for NUL");
    }

    /// A picker that is here: a script that reads its stdin, writes a line
    /// back, and exits with the status it was given.
    ///
    /// The crate's whole job is to spawn something, and the real picker is
    /// not on every machine. The script takes its instructions from its
    /// arguments — so the test can see that the arguments arrived — and it
    /// writes down what it read, so the test can see what the crate sent.
    /// A selection that is printed as lines is one line per record.
    ///
    /// A record that carries a line break would be two lines for whoever
    /// reads it back, and half of a path is a path that does not exist:
    /// the record is left out, and the count says how many were left
    /// out.
    #[cfg(feature = "std")]
    #[test]
    fn a_selection_printed_as_lines_leaves_the_broken_records_out() {
        let args: Vec<String> = Vec::new();
        let mut req = Request::new(&args);
        req.separator = Separator::Line;
        let mut out = Vec::new();
        print_selection(
            &[b"a.txt".to_vec(), b"b\nc.txt".to_vec(), b"d.txt".to_vec()],
            &req,
            &mut out,
        )
        .expect("the selection is printed");
        let text = String::from_utf8(out).expect("the output is text");
        assert_eq!(
            text.lines().count(),
            2,
            "one line for each record that fits: {text:?}"
        );
        assert!(text.contains("a.txt"), "{text:?}");
        assert!(text.contains("d.txt"), "{text:?}");
    }

    /// The rule is the separator's own.
    ///
    /// A record that carries the byte the reader separates on is one the
    /// reader cannot read whole: a line reader splits on the newline, and a
    /// NUL reader splits on the NUL byte. The list the picker was handed is
    /// filtered by the same rule, so a selection that was printed without it
    /// would hand back a record the caller cannot read — and the caller
    /// would read two records where there is one.
    #[cfg(feature = "std")]
    #[test]
    fn a_selection_printed_as_nul_leaves_the_broken_records_out() {
        let args: Vec<String> = Vec::new();
        let mut req = Request::new(&args);
        req.separator = Separator::Nul;
        let mut out = Vec::new();
        print_selection(
            &[b"a.txt".to_vec(), b"b\0c.txt".to_vec(), b"d.txt".to_vec()],
            &req,
            &mut out,
        )
        .expect("the selection is printed");
        let text = String::from_utf8(out).expect("the output is text");
        assert_eq!(
            text.split('\0').filter(|r| !r.is_empty()).count(),
            2,
            "one record for each record that fits: {text:?}"
        );
        assert!(text.contains("a.txt"), "{text:?}");
        assert!(text.contains("d.txt"), "{text:?}");
        assert!(!text.contains("b"), "{text:?}");
    }

    #[cfg(feature = "std")]
    #[cfg(any(windows, unix))]
    struct Fake {
        /// The binary the crate spawns.
        binary: String,
        /// Where the script writes what it read.
        stdin_dump: std::path::PathBuf,
        /// Where the script writes the arguments it got.
        args_dump: std::path::PathBuf,
    }

    #[cfg(feature = "std")]
    #[cfg(any(windows, unix))]
    impl Fake {
        /// Writes the script into `dir`, and says how to run it.
        fn new(dir: &std::path::Path) -> Fake {
            std::fs::create_dir_all(dir).expect("the test directory");
            let stdin_dump = dir.join("stdin.txt");
            let args_dump = dir.join("args.txt");
            let script = dir.join(if cfg!(windows) {
                "picker.cmd"
            } else {
                "picker.sh"
            });
            let body = if cfg!(windows) {
                // `%1` is the status, `%2` the file that gets the stdin,
                // `%3` the file that gets the arguments. A batch file wants
                // the line endings it expects.
                r#"@echo off
>"%~3" echo %*
more > "%~2"
echo PICKED
exit /b %1
"#
                .replace('\n', "\r\n")
            } else {
                r#"#!/bin/sh
echo "$*" > "$3"
cat > "$2"
echo PICKED
exit "$1"
"#
                .to_string()
            };
            std::fs::write(&script, body).expect("the script");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&script)
                    .expect("the script")
                    .permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&script, perms).expect("the script runs");
            }
            Fake {
                binary: script.to_string_lossy().into_owned(),
                stdin_dump,
                args_dump,
            }
        }

        /// The arguments the script takes: the status it exits with, the
        /// file that gets its stdin, the file that gets its arguments, and
        /// whatever the caller of the crate asked for.
        fn args(&self, status: i32, extra: &[String]) -> Vec<String> {
            let mut args = vec![
                status.to_string(),
                self.stdin_dump.to_string_lossy().into_owned(),
                self.args_dump.to_string_lossy().into_owned(),
            ];
            args.extend_from_slice(extra);
            args
        }

        /// The lines the script read.
        fn lines(&self) -> Vec<String> {
            let dump = std::fs::read_to_string(&self.stdin_dump).expect("the dump");
            dump.lines()
                .map(|line| line.trim_end_matches('\r').to_string())
                .filter(|line| !line.is_empty())
                .collect()
        }
    }

    #[cfg(feature = "std")]
    #[cfg(any(windows, unix))]
    #[test]
    fn the_picker_is_spawned_with_the_records_and_the_arguments() {
        let dir = std::env::temp_dir().join(format!("rgy-pick-spawn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = Fake::new(&dir);
        let args = fake.args(0, &["--fzf-arg-value".to_string()]);
        let req = Request {
            args: &args,
            binary: Some(&fake.binary),
            separator: Separator::Nul,
            order: Order::Sort,
        };
        // The records the caller offers: one of them carries a newline,
        // and a picker that reads lines cannot be handed one whole.
        let records = b"src/a.txt\0src/has\nnewline.txt\0src/c.txt\0";
        let search: Search = Box::new(
            move |out: &mut (dyn std::io::Write + Send + 'static)| -> Result<bool> {
                out.write_all(records)
                    .map_err(|e| alloc::format!("the records did not reach the picker: {e}"))?;
                Ok(true)
            },
        );
        let (code, chosen) = pick_records(search, &req).expect("the picker runs");
        assert_eq!(code, 0, "a picker that chose something exits 0");
        assert!(
            chosen.iter().any(|record| record.starts_with(b"PICKED")),
            "the selection must come back; got {chosen:?}"
        );
        // The arguments the caller asked for reached the child.
        let got = std::fs::read_to_string(&fake.args_dump).expect("the dump");
        assert!(
            got.contains("--fzf-arg-value"),
            "the arguments must reach the child; got {got:?}"
        );
        // The records reached the child whole: the record with a newline
        // in it is not offered, and it is not offered in halves either.
        assert_eq!(
            fake.lines(),
            ["src/a.txt".to_string(), "src/c.txt".to_string()],
            "the child must not receive a split record"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A picker that is being written is a picker that is there.
    ///
    /// The kernel refuses to execute a file that is open for writing, and
    /// the refusal is the platform's timing and not the picker's absence:
    /// the crate waits for the platform to let the picker run.
    #[cfg(feature = "std")]
    #[cfg(unix)]
    #[test]
    fn the_picker_runs_when_the_platform_was_holding_it() {
        let dir = std::env::temp_dir().join(format!("rgy-pick-busy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the test directory");
        let script = dir.join("picker.sh");
        std::fs::write(&script, "#!/bin/sh\necho PICKED\nexit 0\n").expect("the script");
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(&script).expect("the script");
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).expect("the script runs");
        }
        // The file is held open for writing, the way a process that is
        // still writing it holds it: the platform refuses to execute it
        // while that lasts.
        let held = std::fs::OpenOptions::new()
            .write(true)
            .open(&script)
            .expect("the hold");
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(held);
        });
        let binary = script.to_string_lossy().into_owned();
        let req = Request {
            args: &[],
            binary: Some(&binary),
            separator: Separator::Nul,
            order: Order::Sort,
        };
        let search: Search = Box::new(
            |_out: &mut (dyn std::io::Write + Send + 'static)| -> Result<bool> { Ok(true) },
        );
        let (code, chosen) = pick_records(search, &req).expect("the picker runs");
        release.join().expect("the hold is released");
        assert_eq!(code, 0, "a picker that chose something exits 0");
        assert!(
            chosen.iter().any(|record| record.starts_with(b"PICKED")),
            "the selection must come back; got {chosen:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "std")]
    #[cfg(any(windows, unix))]
    #[test]
    fn the_exit_status_of_the_picker_is_mapped() {
        let dir = std::env::temp_dir().join(format!("rgy-pick-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = Fake::new(&dir);
        // The crate's own table: a choice is 0, no choice is 1, and a
        // picker that exits with anything else is broken — which the caller
        // answers with 2.
        for (status, expected) in [(0, 0), (1, 1), (130, 1)] {
            let args = fake.args(status, &[]);
            let req = Request {
                args: &args,
                binary: Some(&fake.binary),
                separator: Separator::Nul,
                order: Order::Sort,
            };
            let search: Search = Box::new(
                |_out: &mut (dyn std::io::Write + Send + 'static)| -> Result<bool> { Ok(true) },
            );
            let (code, _) = pick_records(search, &req).expect("the picker runs");
            assert_eq!(code, expected, "the picker exited with {status}");
        }
        // A picker that exits with 2 is not a search with no matches.
        let args = fake.args(2, &[]);
        let req = Request {
            args: &args,
            binary: Some(&fake.binary),
            separator: Separator::Nul,
            order: Order::Sort,
        };
        let search: Search = Box::new(
            |_out: &mut (dyn std::io::Write + Send + 'static)| -> Result<bool> { Ok(true) },
        );
        let reason = pick_records(search, &req).expect_err("a broken picker is an error");
        assert!(reason.contains("exited with 2"), "{reason}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "std")]
    #[cfg(any(windows, unix))]
    #[test]
    fn a_picker_that_cannot_run_is_an_error_and_not_a_crash() {
        let dir = std::env::temp_dir().join(format!("rgy-pick-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the test directory");
        let missing = dir.join("no-such-picker");
        let missing = missing.to_string_lossy().into_owned();
        let req = Request {
            args: &[],
            binary: Some(&missing),
            separator: Separator::Nul,
            order: Order::Sort,
        };
        let search: Search = Box::new(
            |_out: &mut (dyn std::io::Write + Send + 'static)| -> Result<bool> { Ok(true) },
        );
        // The caller answers this error with 2: a picker that could not be
        // run is not a search with no matches.
        let reason = pick_records(search, &req).expect_err("a picker that cannot run is an error");
        assert!(reason.contains("cannot run"), "{reason}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A search that fell over is an error, and not an empty answer.
    ///
    /// The exit status of a search-shaped tool is the whole contract: a
    /// search that matched nothing is one, and a search that broke is
    /// two. A panic reported as "nothing matched" is a lie about what
    /// happened, and it sends the user looking for an answer that was
    /// never there.
    #[cfg(feature = "std")]
    #[cfg(any(windows, unix))]
    #[test]
    fn a_search_that_panics_is_an_error_and_not_an_empty_answer() {
        let dir = std::env::temp_dir().join(format!("rgy-pick-panic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = Fake::new(&dir);
        let args = fake.args(0, &[]);
        let req = Request {
            args: &args,
            binary: Some(&fake.binary),
            separator: Separator::Nul,
            order: Order::Sort,
        };
        let search: Search = Box::new(
            |_out: &mut (dyn std::io::Write + Send + 'static)| -> Result<bool> {
                panic!("the search fell over");
            },
        );
        let reason = pick_records(search, &req).expect_err("a panic is an error");
        assert!(reason.contains("panicked"), "{reason}");
        assert!(reason.contains("the search fell over"), "{reason}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The child the parent test runs: it asks the picker for a selection
    /// and prints it, so the parent can read the bytes the crate printed.
    ///
    /// The crate prints its selection on stdout, and stdout is the one
    /// stream a test cannot take back — so the selection is printed by a
    /// child, and the parent reads the child's stdout.
    #[cfg(feature = "std")]
    #[cfg(any(windows, unix))]
    #[ignore]
    #[test]
    fn prints_the_selection_for_the_parent() {
        if std::env::var_os("RGY_PICK_TEST_CHILD").is_none() {
            // The ordinary run: only the parent sets the variable.
            return;
        }
        let dir =
            std::env::var_os("RGY_PICK_TEST_DIR").expect("the parent says where the picker is");
        let dir = std::path::PathBuf::from(dir);
        let fake = Fake::new(&dir);
        let args = fake.args(0, &[]);
        let req = Request {
            args: &args,
            binary: Some(&fake.binary),
            separator: Separator::Nul,
            order: Order::Sort,
        };
        let records = b"src/a.txt\0";
        let search: Search = Box::new(
            move |out: &mut (dyn std::io::Write + Send + 'static)| -> Result<bool> {
                out.write_all(records)
                    .map_err(|e| alloc::format!("the records did not reach the picker: {e}"))?;
                Ok(true)
            },
        );
        assert_eq!(pick(search, &req).expect("the picker runs"), 0);
    }

    #[cfg(feature = "std")]
    #[cfg(any(windows, unix))]
    #[test]
    fn the_selection_comes_back_in_the_separator_the_caller_asked_for() {
        let dir = std::env::temp_dir().join(format!("rgy-pick-selection-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let exe = std::env::current_exe().expect("the test binary");
        let child = std::process::Command::new(exe)
            .args([
                "--ignored",
                "--exact",
                "tests::prints_the_selection_for_the_parent",
            ])
            .env("RGY_PICK_TEST_CHILD", "1")
            .env("RGY_PICK_TEST_DIR", &dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the test binary runs");
        let out = child.wait_with_output().expect("the child finishes");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "the child must succeed; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        // The picker reads lines; the caller asked for NUL. The selection
        // comes back in the separator the caller asked for.
        assert!(
            stdout.contains("PICKED"),
            "the selection must be printed; got {stdout:?}"
        );
        assert!(
            stdout.contains('\0'),
            "the selection must be printed in the separator the caller asked for; got {stdout:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
