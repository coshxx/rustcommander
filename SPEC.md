# RustCommander – Specification

Dual-pane file manager for Windows. Top priority: **blazing fast** (startup and file operations).

## Performance targets

| Measurement | Target |
|---|---|
| Cold start until first frame with content | < 100 ms |
| Load + display a directory with 10,000 entries | < 50 ms |
| UI thread blocked | never (not even on network drives) |

Measure with `rustcommander --timing` (logs to stderr and `%APPDATA%\RustCommander\timing.log`)
or `rustcommander --bench` (like `--timing`, exits after the first frame with content and reports
the steady-state frame time).

## Technology

- Windows only. Win32 window + Direct2D/DirectWrite, everything custom-drawn.
- Single dependency: the `windows` crate. Release profile: LTO, `panic=abort`, `codegen-units=1`.
- Directory listing: `FindFirstFileExW` with `FindExInfoBasic` + `FIND_FIRST_EX_LARGE_FETCH`,
  always on a background thread, cancellable. Loading indicator only after 150 ms.
- Only visible rows are drawn.
- **Direct2D in software mode** (default). Measured: the hardware render target costs ~150 ms at
  startup (loading D3D + GPU driver) and doesn't draw faster. `RC_HARDWARE=1` switches for comparison.
- `unsafe` lives only in the OS boundary modules (`main.rs`, `window.rs`, `sys/`, `files/listing.rs`,
  `files/dirsize.rs`, `files/watch.rs`, `ops/`, `timing.rs`, `lister/window.rs`). App logic (`app/`, `ui/`, `panel.rs`) is free of `unsafe`.
  The window procedure guards the app against reentrant access (`AppCell`).
- Code structure: see [ARCHITECTURE.md](ARCHITECTURE.md).

### Measurements v0.1 (release build, 120 dpi)

| | Result |
|---|---|
| Cold start until frame with content | ~40–50 ms (hardware target: ~175 ms) |
| Load + sort 10,000 entries | 6–8 ms |
| `C:\Windows\System32` (5,000 entries) | ~2 ms |
| Frame time 1186×722 DIP / 2752×1081 DIP | ~7 ms / ~12.5 ms |

## Look

- Custom dark theme; all colors centralized in `theme.rs` (light theme comes later).
- Standard Windows title bar, dark-tinted via DWM.
- Font: Segoe UI Variable; icons are glyphs from Segoe Fluent Icons, colored by file type.
- Columns: Name (incl. extension) | Size | Date. (Later configurable: extension as its own column.)
- Sorting: `..` first, directories before files, natural & case-insensitive.
- Hidden/system files: dimmed.
- UI language: German for now.

## Controls (v0.1)

| Key | Action |
|---|---|
| Tab | Switch pane |
| ↑ ↓ PgUp PgDn Home End | Move cursor |
| Enter / double-click | Open directory or archive, or open file via shell (from an archive: extracted to temp first) |
| Shift+Enter | Open with the default program, also archives |
| F3 | View the file in a lister window (see below; from an archive: extracted to temp first) |
| Ctrl+PgDn | Enter directory or archive (nothing else) |
| Backspace, Ctrl+PgUp | Parent directory |
| \ | Drive root (also works on German keyboard layouts) |
| Alt+F1 / Alt+F2 | Choose drive for left / right pane |
| Ctrl+L, click on path | Edit path (Ctrl+V, environment variables) |
| Ctrl+R, F2 | Reload |
| Type letters | Quick search (jumps to first match), Esc cancels |
| Insert, Space | Toggle mark and move down (Space only when no quick search is active) |
| Space on a folder | Also counts the folder's size, shown in the size column (see below) |
| Alt+Shift+Enter | Count the size of every folder in the pane |
| Shift+↑ / Shift+↓ | Toggle mark and move |
| Ctrl+A | Mark all / unmark all |
| Num+ / Num− | Mark / unmark by file mask (small dialog, last mask preselected) |
| Ctrl+Num+ / Ctrl+Num− | Mark all / unmark all, no dialog |
| Num* | Invert marks of files |
| F5 / F6 | Copy / move marked entries (or the one under the cursor) to the other pane |
| Shift+F6 | Rename |
| F7 | New directory (`a\b` creates nested directories) |
| F8, Del | Move to recycle bin (with confirmation) |
| Shift+F8, Shift+Del | Delete permanently (with confirmation) |
| Esc (while a job runs) | Cancel the job |
| Esc (otherwise) | Cancel running folder size counts |

### Mouse

| Action | Effect |
|---|---|
| Click / double-click | Set cursor / open |
| Right click | Set cursor and toggle mark |
| Right drag | Every row passed gets the state of the first one; scrolls automatically past the list's edges |
| Wheel | Scrolls the view only; the cursor stays on its entry (may leave the view). The next key press brings it back. |
| Scrollbar | Drag the thumb; click the track to page towards the mouse (auto-repeat after the system keyboard delay, stops at the mouse). Wider and brighter on hover. |

Auto-refresh keeps the scroll position as well; only navigation scrolls to the cursor.

On startup the last directories and the window position are restored
(`%APPDATA%\RustCommander\config.ini`).

### Folder sizes (Space, Alt+Shift+Enter)

- Counted in the background by a shared pool of worker threads (one per core, 2–16) that take
  directories from one stack, so wide trees use every core and several folders can be counted at
  once. Each directory is read with `FindFirstFileExW` (`FindExInfoBasic`, large fetch); the
  sizes come with the listing, no file is opened.
- Logical file size (like Explorer's "Size"). Directory links (junctions, directory symlinks) are
  not followed; other reparse points (OneDrive placeholders …) are ordinary folders.
- The number grows live (repainted every 100 ms, accent color) and turns into the normal color
  when done. `≥ 1,2 GB` means something could not be read (access denied).
- Counted sizes go into the "N markiert · size" sum of the status bar.
- They stay across automatic refreshes and are dropped when leaving the directory or reloading
  with F2 / Ctrl+R. No re-sorting.
- Inside archives the size comes from the table of contents.
- Measured (warm cache, 16 threads): `C:\Windows` (45 GB) in 1.6 s; single-threaded 7.7 s.

### File masks (Num+ / Num−)

`*` and `?`, case-insensitive; several masks separated by `;` or spaces (`"…"` keeps spaces);
`|` starts the excludes (`*.* | *.bak`). Masks apply to files only, unless they end in `\`
(`*\` = all directories). A mask without wildcards means "contains". `*.*` also matches names
without a dot. Num+ adds to the existing marks, Num− only removes matches. The main keyboard's
`+`/`-` stay quick search characters. The last mask is saved in `config.ini`.

## File operations

- Run on a worker thread (`ops.rs`); progress card appears after 200 ms (files, bytes, %).
- Copy/move destination: an existing directory, or a path ending in `\`, means "into";
  a single source with a non-existing destination means "copy/move with a new name".
- Conflicts ask: overwrite / skip / overwrite all / skip all / cancel.
- Copy uses `CopyFileExW` (cancellable mid-file). Same-volume move is a rename;
  cross-volume move copies each file and deletes the source only after a successful copy.
- Moving onto an existing directory merges the contents.
- Permanent delete never follows junctions/symlinks (the link is removed, not the target's
  contents). Read-only files are handled.
- Recycle bin via `SHFileOperationW` with `FOF_WANTNUKEWARNING`: if a file can't be
  recycled (e.g. network drive), the shell asks before deleting it for good.
- Copy into itself / onto itself is refused.
- Panes refresh automatically on directory changes (`FindFirstChangeNotificationW`, throttled
  to one reload per 250 ms); cursor, scroll position and marks survive the reload.
- Tests: `cargo test` runs the operations against throw-away directories in `%TEMP%`.
  `RC_CONFIG_DIR` points the app to a separate config (used for GUI tests).

## Archives

- zip, 7z, rar, tar (also `.tar.gz/.tgz/.tar.bz2/.tbz2/.tar.xz/.txz/.tar.zst`), cab and iso open
  like directories (detected by extension; Office files, `.jar`, `.apk` etc. open normally).
  Path display like Total Commander: `C:\Downloads\fotos.zip\urlaub`; works in the path bar and is
  restored on startup.
- Reading uses libarchive as shipped with Windows 11 (`archiveint.dll`, loaded from System32 on
  first use). Without it (Windows 10) entering an archive shows an error.
- Read-only: F5 extracts (progress, cancel, conflict question like copying; file times are kept).
  F6/F7/F8/Shift+F6 inside an archive and copying into one show "Archive sind schreibgeschützt".
- Entry paths that would leave the destination (`..`, absolute paths, drive letters) are neither
  listed nor extracted.
- Enter on a file inside an archive extracts it to `%TEMP%\RustCommander\<session>\<n>\` and opens
  it. The session directory is removed on exit (files still open elsewhere stay); leftovers older
  than a day are removed at startup. Nothing outside `%TEMP%\RustCommander` is deleted.
- The table of contents is read once and cached until the archive file changes; the panel
  watches the directory that holds the archive.
- Encrypted archives: the listing is shown if readable; extracting encrypted entries reports
  "Passwörter werden noch nicht unterstützt".

## Lister (F3)

A separate, non-modal window per file, like Total Commander's Lister; any number can be open.
Closing the main window closes them.

- **Instant for any size:** the file is memory-mapped (shared read/write/delete, so other
  programs are never blocked) and only the visible lines are decoded. The position is a byte
  offset, not a line number, so no pass over the whole file is needed; the scrollbar maps to the
  byte position. Lines longer than 4096 characters are broken even without wrapping; scrolling up
  into a line longer than 64 KB may start it at an arbitrary point.
- **Modes:** `1` text (line breaks, wrap at the window edge), `2` binary (80 bytes per line, line
  breaks ignored), `3` hex (offset, 16 bytes, characters). The start mode is guessed from the
  first 64 KB: NUL bytes or many control codes → hex, otherwise text.
- **Encoding (text):** byte order mark → UTF-8 / UTF-16 LE / BE; UTF-16 without BOM is recognized
  by its zero bytes; otherwise UTF-8 if valid, else ANSI. `A` ANSI, `S` DOS (OEM code page, with
  the DOS glyphs), `u` UTF-8, `U` (Shift+U) UTF-16. Binary and hex mode show ANSI or DOS.
- **Keys:** ↑ ↓ PgUp PgDn Home End, ← → (Ctrl: half a page) scroll sideways when lines don't
  wrap, `W` wrap on/off (saved), wheel, scrollbar (drag the thumb, click the track to page).
  Esc or `Q` closes; F3 closes as well until a search was made in the window.
- **Search:** F7 or Ctrl+F opens a small dialog (last term preselected); Alt+G toggles "match
  case" (default: ignore case, also for umlauts). Searches forward from the match on screen or
  from the top line; F3 next, Shift+F3 previous. Runs on a background thread over the raw bytes
  (the term is encoded in the shown encoding; binary/hex search ANSI/DOS text); the status bar
  shows the progress, Esc cancels. The match is highlighted and scrolled into view.
- **Status bar:** mode, encoding, wrap, file size, position in %, hex offset; key hints.
- Window position and wrapping are saved in `config.ini` (`lister_window`, `lister_wrap`).

## Deliberately later

Lister: selecting and copying text, images, following growing files, N/P for the next/previous
file, line numbers, hex search (`4D 5A`). Archives: packing (Alt+F5) and writing into archives, password dialog, archives inside archives,
7-Zip's `7z.dll` as a fallback for Windows 10. Parallel copying of many small files, shell context
menu (long right press / Shift+F10, since a right click marks), system icons (async, cached), live filter, light theme, settings, normalizing path casing
(typed `c:\windows` → `C:\Windows`), partial redraws (if frame time on large windows becomes an issue).
