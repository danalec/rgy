# rgy — the formats

What rgy can find, format by format: the names it knows, the bytes it can
read, and what it reads *inside* a container through the format adapters.

The list of families, and what each one gives, is in this file. The
[README](README.md) is the map: what rgy is, how to run it, and where the
docs live.

The gaps are at the end of this file. A format that is silently unsearchable
is worse than one that is documented as such.

## What it reads today

Three things, and they are three different questions.

**The name.** rgy walks the filesystem — the MFT on Windows, `getdents64`
on Linux, `getattrlistbulk` on macOS, `getdirentries` on FreeBSD — and
indexes every file and folder by name. That index is the fast path: a query
like `invoice ext:pdf` is
answered from the index without reading a single file's contents. This is
the part of rgy that is finished and measured.

**The bytes.** `grep:`, `regex:` and `sym:` read the file and search its
bytes, line by line. The output is `path:line:text`, the same shape `rg`
prints. A file the format adapters claim is read *through* them — a PDF's
text layer, a `.docx`'s paragraphs, a zip's members — and the rendering is
what the search sees. A file no adapter claims is read as bytes, which is
what a `.txt` has always been.

> **Untrusted input.** Those adapters are linked into the search process;
> they are not sandboxed. A PDF, zip or office file from somebody else is
> hostile input to a decoder. The read is capped at 4 MiB, nesting at four
> levels with 4096 members per level, and the compression decoders have an
> output cap — but there is no process isolation and no timeout on one
> parse. Set `RGY_ADAPTERS=off` to read only a file's own bytes and invoke
> no parser at all; name search is unaffected. The full list of bounds and
> omissions is in the [manual](MANUAL.md#untrusted-files-and-the-format-adapters).

**The corpus.** A pack store (`searchd --tv-dir`, or `rgx-tv build`) ingests
a tree once into verified packs, and content queries then run against the
packed bytes. Same bytes, same answers, but a corpus that does not change
is read once instead of once per query. `rgx-tv` is upstream's tool and it
is not in the release archive — build it from the source if you want it.
See the [manual](MANUAL.md) for the flags.

```
searchctl --root ~ "invoice ext:pdf"        # the name
searchctl --root ~ "invoice grep:tesla"     # the bytes
searchctl --root ~ "invoice ext:pdf grep:tesla"   # both, one query
```

## What the container search adds

A PDF's text is not in the file as text: it sits in compressed object
streams. A `.docx` is a zip with XML inside. A `.mbox` holds a stack of
messages. The adapters read those structures and hand back searchable text
— plus the metadata a file carries about itself.

The adapter crates are already in this workspace, transported verbatim from
[rgx](https://github.com/danalec/rgx), and they are wired: the content path
asks the registry what a file renders to, and the answer is searched like the
file's own bytes. The table below is therefore a list of what answers today.

| family | formats | state |
|---|---|---|
| archive | zip (36 extensions, among them `.gem`, `.nupkg`, `.crate`) · 7z · tar · cpio · ar · deb · git-pack · git bundle · Subversion dump · rar (store and RAR5) · gz bz2 xz zstd lz4 brotli, including `tar.*` · a PostgreSQL custom dump (`.dump`) · a borg segment (no extension at all: `data/0/2` is claimed by its magic) · the iWork packages (`.pages`, `.numbers`, `.key`), which are zip containers whose *members* are searchable but whose document lives in a compressed IWA archive | wired: read through the adapter |
| images | ISO 9660 (Joliet and Rock Ridge names, files extracted) · SquashFS 4 · WIM · VHD · VHDX · QCOW2 · VMDK · VDI · EROFS · APFS · HFS+ (catalog names) · DMG · a raw `.img` dispatched by its own table | wired: read through the adapter |
| office | docx · xlsx · pptx · ODF · epub · vsd / vsdx · xps · rtf · legacy `.doc/.xls/.ppt` · WordPerfect · AbiWord · Hangul `.hwd` · LaTeX · the lightweight markup family (markdown, HTML, RST, org-mode, MediaWiki, Textile, txt2tags, Jupyter notebooks, troff man pages) — dark by default, see the note below the table | wired: read through the adapter |
| pdf | text in-process: xref streams, object streams, CID fonts, the filter zoo · the text rendering mode is tracked, so a hidden layer is named and a document whose whole layer is invisible hands its page images over for OCR · encrypted PDFs under the standard security handler when the user password is empty | wired: read through the adapter |
| llm | gguf · safetensors · npy · onnx · protobuf · tiktoken · keras · torch · pickle | wired: read through the adapter |
| tabular | sqlite · duckdb · dbf (and the Visual FoxPro `.dbc`) · SYLK · DIF · Lotus 1-2-3 · Microsoft Access · LMDB `data.mdb` · shapefile · parquet · arrow · ERI · mat v5 · netCDF-3 · NASA CDF · hdf5 (and the netCDF-4 and MATLAB v7.3 files that are HDF5 underneath) · MongoDB BSON · Redis RDB and AOF · LevelDB / RocksDB · Zarr | wired: read through the adapter |
| media | EXIF (jpeg/tiff/png/webp) · GIF and BMP headers · PSD · JPEG 2000 · JPEG XL · OpenEXR · audio tags · video headers, Matroska chapters and tags · fits · dicom · font tables · e-book metadata (MOBI/AZW/AZW3/PRC) | wired: read through the adapter |
| 3d | glb · stl · ply · fbx · pcd · las and laz (the LASzip-compressed twin, decoded point for point) · vtk · blend · 3ds · NIfTI volumes · MRC/CCP4 volumes | wired: read through the adapter |
| mail | mbox · eml · emlx · msg · pst/ost · wab (Windows Address Book) · MMDF mailboxes · Thunderbird `.msf` indexes · Apple mailbox index — attachments recurse through the chain | wired: read through the adapter |
| inspect | forensic triage on any binary: magic ID, structure carving, strings, entropy, x509 | wired: read through the adapter |
| inspect · documents | OLE2/CFB compound files · binary property lists · multi-page TIFF and raw camera formats · DjVu · ISO-BMFF (`ftyp`/`meta`/`ilst`) · Windows shell links (`.lnk`) · CALS rasters · JEDMICS C4 | wired: read through the adapter |
| inspect · databases | Firebird and InterBase header pages (the rows live behind `RDB$PAGES` and are not walked) | wired: read through the adapter |
| inspect · executables | PE `.rsrc` · Mach-O `__cstring`/`__cfstring` · ELF `.comment` and `NT_GNU_BUILD_ID` · DEX string pool · WASM `name`/`producers` | wired: read through the adapter |
| inspect · science | GRIB 1/2 · BUFR · SEG-Y · CERN ROOT · N5 · N5z | wired: read through the adapter |
| inspect · CAD | DWG · Parasolid transmit files · ACIS `.sab` · Revit `.rvt` · MicroStation DGN · and the text CAD formats — DXF, IFC/STEP, IGES, ACIS `.sat`, STL — which are *rendered*: the file's own bytes stay the default answer, and the rendering appears under `RGY_ADAPTER_VIEW=render` | wired: read through the adapter |
| inspect · graphics | plotter drawings (HP-GL) — *rendered* the same way · graphics metafiles (CGM) — *revealed*, since the file is binary | wired: read through the adapter |
| inspect · genomics | BAM · BCF 1 and 2 · CRAM 3 · and the flat files of the same family — FASTA, FASTQ, VCF, GFF/GTF, PDB, mmCIF — which are *rendered* | wired: read through the adapter |
| docs | OCR through an external `tesseract` on `*.ocr.jpg`, `*.ocr.png`, `*.ocr.pdf` — a scanned PDF's page images are handed over, so the claim works on any tesseract build · a faxed page (CCITT G3/G4) and a JBIG2 page decoded in-house · `.pub` through `soffice` | wired: read through the adapter |

Adapters extract searchable structure, not payloads: model metadata and
tensor shapes, EXIF and tag values, schemas and rows. Plain-text formats
(csv, json, log, …) need no adapter and are searched raw today.

**Some text formats have an adapter and it is dark by default.** `.html`,
`.md`, `.rst`, `.org`, the LaTeX trio, the troff man pages, `.rtf` and the
rest of the markup family are line-oriented text that a byte search already
reads perfectly, and every one of those extractors is *lossy* on it — the
HTML arm drops `<script>`, `<style>` and `<head>` bodies, the Markdown arm
drops front matter and link targets. So the file's own bytes stay the
default answer and the rendering appears only when it is asked for, under
`RGY_ADAPTER_VIEW=render`. The same rule covers the text CAD formats,
the plotter drawing, and the flat files of the genomics family: a rendering
is not obliged to be a faithful copy — joining a FASTA's wrapped sequence
puts `chr1 11869` next to each other, a pattern the file does not contain.
The switch is `RGY_ADAPTER_VIEW`, and rgx's `RGX_ADAPTER_VIEW` is the
fallback: rgy reads its own name first, so a shell that exports rgx's gets
the same behaviour here, and a shell that exports rgy's gets rgy to differ
from rgx without touching rgx. The content path reads through whichever
name is set. The default keeps the file's own bytes for those formats,
which is why they are dark, and a format whose bytes are not text at all — a
PDF, a zip — is read through its adapter either way: there the rendering is
the only text there is.

## The facts each format gives

A *fact* is a name and a value, and the value is the one the adapter read
out of the file. This is the catalogue, and it is what a filter will ask
about.

| fact | where it comes from |
|---|---|
| `path` | the file's path, or the logical path inside a container |
| `ext` | the extension of that path |
| `format` | the adapter that answered for the file (`raw` when none did) |
| `mime` | the MIME type the extension claims |
| `lang` | the language the extension claims |
| `container` | the kind of container a member is inside — `zip`, `7z` |
| `archive.entry` | how many entries the container holds |
| `author`, `title`, `subject`, `keywords`, `creator`, `producer`, `creationdate`, `moddate` | a PDF's `/Info` dictionary |
| `mail.from`, `mail.to`, `mail.subject`, `mail.date` | a message's headers |
| `mail.messages` | how many messages an mbox holds |
| `has.attachments` | present only when the message really has attachments |
| `exif.make`, `exif.model`, `exif.camera`, `exif.gps`, `exif.datetime` | a photo's EXIF block |
| `tensor.name`, `tensor.shape`, `tensor.dtype` | a safetensors model's tensors |
| `tensor.count` | how many tensors a GGUF header declares |
| `table.name`, `table.column` | a table's columns — sqlite, parquet, dbf |
| `zarr.shape`, `zarr.chunks`, `zarr.dtype`, `zarr.compressor`, `zarr.order` | a Zarr array's manifest |

rgy's filters today are the name-level ones — `ext:`, `type:`, `dir:`,
`size:`, `mtime:`, `in:`, `re:` — and the content ones, `grep:`, `regex:`
and `sym:`. The fact predicates above are the vocabulary the adapters add
to that list when they are wired: `author:tesla` beside `ext:pdf`, the
question "which files does this corpus hold that were written by Ada
Lovelace?" asked of the bytes rather than of the names.

## A PDF

The interesting case, because a PDF is three different things at once.

The **text layer** is compressed object streams, and the reader
decompresses them, follows the xref table and the xref streams, and reads
the fonts — including the CID fonts, where a character is an index into a
table the reader has to resolve.

A **scanned** PDF has no text layer at all: the page is an image. Its page
images are handed to an external `tesseract`, and what OCR returns is the
text. The claim is the same on any tesseract build, because the OCR is
outside the reader.

An **encrypted** PDF is read when the user password is empty, which is the
overwhelmingly common case and the only one a search tool can answer
without a secret. A file that needs a password is declined: its bytes stay
searchable, and the `/U` check decides — a key that does not reproduce `/U`
is never used.

What the reader gives: the document's text, and its `/Info` dictionary —
author, title, subject, keywords, creator, producer, the two dates.

## A Word or spreadsheet document

A `.docx` is a zip. The reader opens the zip, finds the document part, and
reads the XML inside it — the paragraphs, the tables, the headers. The
legacy `.doc` is a different animal: the OLE2 compound file, with the text
in a piece table that the reader walks. The two are the same question
("what does this document say?") with completely different machinery.

A spreadsheet is the same story with cells instead of paragraphs: the
reader gives the sheet names, the columns, and the values.

## An archive

A zip is a table of contents and a run of compressed members. The reader
gives the member names and recurses into them, so a `.docx` inside a `.zip`
is found by name and read as a document.

A member is *named*, not addressed: `bundle.zip!script.py` names the
member, but nothing in that path says where inside the member a hit is. A
PDF is one text, so `archive.zip!doc.pdf` names no page, and a
spreadsheet's sheet and a cell's address are not in the path at all. The
`!` is reserved, so a file called `report.txt!draft` is read as the
container `report.txt` and the member `draft`.

## A mailbox

An `.mbox` is a text file holding a stack of messages; the reader splits
them and reads each one's headers. A `.pst` is not: it is a database with
its own index structures, and the reader walks that index to find the
messages and then recurses into their attachments.

What the reader gives: `from`, `to`, `subject`, `date`, how many messages
the box holds, and whether a message really has attachments.

## A photo

The EXIF block of a JPEG, TIFF, PNG or WebP: the camera, the model, the
GPS coordinates, the moment it was taken. A raw camera file — `.nef`,
`.dng`, `.cr2` — is a TIFF under another name, and is read as one.

## A model file

A safetensors model is a header of tensor names, shapes and dtypes
followed by the weights. A GGUF file declares its tensors in the header
and keeps the data in blocks. The reader gives the names, the shapes and
the dtypes — the question a search tool is asked about a model is "what is
in it", and not "what does the weight tensor contain".

## A 3D file

A `.glb` is a binary container with a JSON chunk and a binary chunk. An
`.stl` is a triangle soup. A `.ply` is a header of properties and then
vertices. The reader gives the names in the scene: the nodes, the meshes,
the materials, the cameras.

A `.laz` is a LAS file compressed with LASzip: the reader decompresses it
point by point, so a LAS and its compressed twin answer the same.

## Where the formats are specified

Each format has a document that says what its bytes are, and an
implementation that writes them.

The long version is rgx's own
[FORMATS.md](https://github.com/danalec/rgx/blob/main/FORMATS.md) — the
sibling repository's copy of this document, where each format gets a
section of its own and where each reading is checked against the oracle
the format's own producer ships, not a second implementation of the same
idea. This file is the summary of that one.

The [short version](#where-the-formats-are-specified) of where the
specifications live:

| format | the specification |
|---|---|
| PDF | the ISO 32000 PDF specification, revisions 1.7 and 2.0 |
| ZIP | PKWARE's APPNOTE, and the ZIP specification's history |
| 7z / LZMA | the LZMA SDK's format description (public domain) |
| RAR | the RAR 5.0 format description, and the 2.9/3.x one for the old store method |
| TIFF / EXIF | TIFF 6.0 and the Exif 2.31 specification |
| HDF5 | the HDF5 File Format Specification, versions 1.0-4.0 |
| netCDF | the netCDF classic and CDF-2/5 format descriptions |
| GRIB | the WMO Manual on Codes, No. 306, Vol. I.2 Part B |
| SQLite | the file format description in SQLite's own documentation |
| LAS / LASzip | the LAS 1.4 specification and the LASzip description |

## Known gaps

Stated rather than papered over.

- **The facts are not in the filters yet.** The adapters answer with the
  structure they can read — a PDF's text layer, an archive's members, a
  metadata family's tags — and `grep:` searches that. The *filters* still ask
  the name index only: `ext:`, `type:`, `dir:` and `size:` do not know what a
  container holds.

- **A member is named, not addressed.** The recursion goes four container
  levels deep by default, and each member carries a logical path — but the
  path does not say where inside the member a hit is. A PDF is one text, a
  spreadsheet has sheets and cells, a paragraph has an index: none of that
  is in the path.

- **Encrypted PDFs need an empty user password.** The standard security
  handler is implemented in full, but only the case where the user
  password is empty — the common one, and the only one answerable without a
  secret. A file that needs a password is declined, its bytes stay
  searchable, and revision 5 is declined rather than guessed at.

- **A page is rasterised by an external tool.** Text drawn with `3 Tr` (or
  `7 Tr`) is invisible on the page, and the reader follows the rendering
  mode through the content stream: the same page with `0 Tr` and with `3 Tr`
  no longer answers identically. A document whose whole layer is invisible
  hands its page images over for OCR instead.

- **Two tools live outside the reader.** OCR needs `tesseract` installed,
  and `.pub` needs `soffice`. Both are probed once and skipped when
  absent, so a machine without them searches less, not differently.

- **Some formats are deliberately left to the byte search.** FASTA, FASTQ,
  GFF, GTF, SAM, VCF, PDB, mmCIF and the desktop pointers (`.gdoc`,
  `.desktop`, `.url`) are line-oriented text that the byte search already
  reads perfectly. An adapter that answered for them would strip nothing
  and add nothing.

- **A `.tflite` is not on any list, and that is not a gap.** A TFLite
  model is FlatBuffers, so a protobuf reader would either refuse it or —
  worse — accept a buffer and print field names that do not exist. Leaving
  it unclaimed is a decision: the file falls through to the byte search and
  keeps the answer it always had.

- **An encrypted container needs a secret.** A borg repository, a restic
  one, a kopia one: the format is documented and the reader is written,
  but an encrypted repository needs a passphrase, and a search tool may not
  ask for one.

- **The formats rgy does not know are not a special case.** A file the
  adapters do not claim falls through to the byte search and keeps the
  answer it always had.
