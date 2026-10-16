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
- `build.rs` embeds the icon (`res/app.rc`, `res/app.ico`) with `rc.exe` from the Windows SDK (no
  crate; `RC` overrides the path). Without the SDK the exe is built without icon.
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
- Standard Windows title bar, dark-tinted via DWM. Title `RustCommander <version from
  Cargo.toml>`, debug builds add `(Debug)`.
- Icon: white bold "RC" on a rounded square in the accent blue, 16–256 px; drawn by
  `res/make-icon.ps1` (GDI+), used by the exe, the main window and the lister windows.
- Font: Segoe UI Variable; icons are glyphs from Segoe Fluent Icons, colored by file type.
- Columns: Name (incl. extension) | Size | Date. (Later configurable: extension as its own column.)
- Sorting per panel by name, size or date (click on a column header or Ctrl+F3 / Ctrl+F6 /
  Ctrl+F5; again = other direction). A new column starts A→Z, largest or newest first. `..`
  first, directories before files; ties and names natural & case-insensitive. Without the option
  "Ordner in die Sortierung einschließen" folders are always sorted by name (descending only when
  sorting by name descending). The sort column shows ▲/▼; saved per panel in `config.json`.
- Hidden/system files: dimmed.
- UI language: English or German (see Languages below).
- Key bar: the most used keys; a click on an entry does what its key does. While Alt is held it
  shows the Alt keys instead (Alt+F1/F2 drives, Alt+F4 quit, Alt+F5 pack, Alt+F7 search, Alt+F9
  unpack); AltGr does not switch it.

## Controls (v0.1)

| Key | Action |
|---|---|
| Tab | Switch pane |
| ↑ ↓ PgUp PgDn Home End | Move cursor |
| Enter / double-click | Open directory or archive, or open file via shell (from an archive: extracted to temp first) |
| Shift+Enter | Open with the default program, also archives |
| F4 | Edit the file in the editor (Notepad, or `"editor"` in `config.json`); not inside archives |
| Shift+F4 | Edit a file by name; created empty if it doesn't exist (dialog preset with the file under the cursor) |
| F3 | View the file in a lister window (see below; from an archive: extracted to temp first) |
| Ctrl+PgDn | Enter directory or archive (nothing else) |
| Backspace, Ctrl+PgUp | Parent directory |
| \ | Drive root (also works on German keyboard layouts) |
| Alt+F1 / Alt+F2 | Choose drive for left / right pane (where it lands: see "Switching drives" below) |
| Ctrl+L, click on path | Edit path (Ctrl+V, environment variables) |
| Ctrl+R, F2 | Reload |
| Type letters | Quick search (jumps to first match), Esc cancels; with the option "Alt+letter" only Alt+letter starts it |
| Insert, Space | Toggle mark and move down (Space only when no quick search is active; see `space_stays`) |
| Space on a folder | Also counts the folder's size, shown in the size column (see below) |
| Alt+Shift+Enter | Count the size of every folder in the pane |
| Shift+↑ / Shift+↓ | Toggle mark and move |
| Ctrl+A | Mark all / unmark all |
| Num+ / Num− | Mark / unmark by file mask (small dialog, last mask preselected) |
| Ctrl+Num+ / Ctrl+Num− | Mark all / unmark all, no dialog |
| Num* | Invert marks of files |
| F5 / F6 | Copy / move marked entries (or the one under the cursor) to the other pane |
| Shift+F6 | Rename (dialog; inline renaming: slow second click, see Mouse) |
| F7 | New directory (`a\b` creates nested directories) |
| F8, Del | Move to recycle bin (with confirmation) |
| Shift+F8, Shift+Del | Delete permanently (with confirmation) |
| Esc (while a job runs) | Cancel the job |
| Esc (otherwise) | Cancel running folder size counts |
| Ctrl+F3 / Ctrl+F6 / Ctrl+F5 | Sort by name / size / date; again = other direction |
| Shift+F10, menu key | Shell context menu (Shift held: extended entries) |
| Alt+F5 | Pack the marked entries into a zip or 7z archive (see Archives) |
| Alt+F7 | File search (see below) |
| Alt+F9 | Unpack the marked archives (see Archives) |
| Ctrl+, | Options (see below); also the gear button at the right end of the key bar and "Options…" in the window menu |

### Command line (option)

- With **Show command line** (`command_line`, default off) a line sits between the status bars
  and the key bar: the active pane's path as prompt (`C:\Intel>`), then the input. Clicking into
  it places the caret. Like Total Commander, typing goes there; the quick search then needs
  Alt+letter (the Alt option is greyed out meanwhile), a running quick search still takes plain
  characters.
- ↑/↓, PgUp/PgDn, Home/End and Insert keep steering the pane, ←/→ move the caret. With an empty
  line Enter, Backspace, Del, Ctrl+A, Space, `\` and Shift+Enter do what they always do; with
  text Enter runs it, Backspace/Del edit, Ctrl+A selects, Esc clears. Ctrl+V pastes.
- Enter runs the line with `cmd /c` in a new console window in the active pane's folder (inside
  an archive: the folder holding it); the window closes when the command is done. Shift+Enter
  uses `cmd /k`, the window stays open.
- Handled by the pane instead: `D:` (switch drive, like the drive list: see "Switching drives"
  below; `D:\` always goes to the root), `cd <path>` (also `cd..`, `cd\`, `cd /d`, relative, `%VARS%`) and a bare path of an
  existing folder. A missing drive or folder shows a notice and keeps the text; otherwise the line
  is cleared.
- Ctrl+Enter inserts the name under the cursor, Ctrl+Shift+Enter its full path (quoted if they
  contain spaces, followed by a space).
- History: the last 50 lines (internal commands too, a repeated one moves up), Ctrl+↑/↓ browse
  them. Saved in `history.json` next to `config.json`: `{ "commands": [newest, …] }`.

### Switching drives

The drive list (Alt+F1/F2, click on the drive chip) and `D:` in the command line pick the folder
on that drive like this:

1. The pane is on that drive already: it stays where it is.
2. The other pane is on that drive: its folder (inside an archive: the folder holding it).
3. Else the folder this pane was in last on that drive. If it is gone, quietly its nearest
   existing parent (tried in the background, a slow network drive doesn't block).
4. Else the root.

Each pane remembers its last folder per drive letter (not UNC paths), saved as `left_drives` /
`right_drives` in `config.json`. `\` and `D:\` always go to the root.

### Mouse

| Action | Effect |
|---|---|
| Click / double-click | Set cursor / open |
| Slow second click in the name column of the entry under the cursor | Rename in place (see below) |
| Left drag (past the system drag threshold) | Drag & drop out of the app (Explorer, desktop, mail clients, browsers like web Gmail): the row, or all marked rows if it is one of them; the marks stay. The target picks copy, move or link (Explorer's defaults: move on the same drive, Ctrl copies). Not on `..` or inside archives. |
| Drop files onto a panel (from the other panel, Explorer, the desktop …) | Opens the copy dialog (Shift held on release: the move dialog) with the target filled in: the folder row under the mouse (outlined; not `..`), else the panel's directory (the list is outlined). The dialog runs like F5/F6; dragged from a panel, its marks are cleared afterwards like with F5. Only real files (no virtual files like mail attachments, no text or links). The source is always told "copied", so it never deletes anything itself. Refused (no-drop cursor) while a job runs or a dialog is open, over archives, the status bars, the command line and the key bar, and into the directory the files are in, onto one of them or into a folder inside them. |
| Right click | Set cursor and toggle mark |
| Right drag | Every row passed gets the state of the first one; scrolls automatically past the list's edges |
| Right press held 0.5 s on a row | Shell context menu (the row's mark is undone) |
| Wheel | Scrolls the view only; the cursor stays on its entry (may leave the view). The next key press brings it back. |
| Scrollbar | Drag the thumb; click the track to page towards the mouse (auto-repeat after the system keyboard delay, stops at the mouse). Wider and brighter on hover. |

Auto-refresh keeps the scroll position as well; only navigation scrolls to the cursor.

Inline renaming, like Explorer: a click anywhere in the name column of the entry that already has the cursor
in the active pane turns the name into an input 300 ms later. Two clicks less than 300 ms apart
are a double click (open), even though Windows' own double-click time is longer (usually 500 ms,
which felt sluggish); a key press or mouse move cancels; never on `..` or
inside archives). Files start with the name before the extension selected, folders entirely;
Shift+←/→/Home/End extend the selection, Ctrl+A selects all (in every text input). Enter, Tab,
a click elsewhere, the wheel or another window taking the focus rename; Esc cancels. An unchanged
or empty name changes nothing, a case-only change renames. On an error (invalid name, target
exists, access denied) the notice shows it and the input stays open. Afterwards the cursor is on
the renamed entry.

On startup the last directories and the window position are restored
(`%APPDATA%\RustCommander\config.json`, see Settings below).

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
  with F2 / Ctrl+R.
- With the option "Ordner in die Sortierung einschließen" and sorting by size, counted folders are
  sorted by their size; uncounted ones (and those still counting) come last in the folder block,
  by name. Nothing is ever counted automatically. When a count finishes the list is sorted again;
  the cursor stays on its entry.
- Inside archives the size comes from the table of contents.
- Measured (warm cache, 16 threads): `C:\Windows` (45 GB) in 1.6 s; single-threaded 7.7 s.

### Settings (Ctrl+,)

- Opened with Ctrl+, the gear button at the right end of the key bar (always visible, hover shows
  "Options (Ctrl+,)"; clicking it again closes the dialog) or "Options…" in the window menu
  (title bar icon / Alt+Space).
- Options dialog: the language, then one checkbox per option; ↑/↓ select, Space/Enter/click toggle
  (on the language row: open the list of languages), Esc or a click outside closes. A change takes
  effect and is saved immediately (no OK/Cancel).
- **Language** (`language`): see Languages below.
- **Show exact sizes in bytes** (`exact_sizes`, default off): size column (files and counted
  folders, also while counting) and status bar show exact byte counts with thousands separators
  (`48,318,382,080` / `48.318.382.080`, no unit; `≥` keeps its meaning). The size column is then 140
  instead of 84 DIPs wide. Progress card and lister stay rounded.
- **Include folders in the sort order** (`sort_dirs`, default off): folders (still above the
  files) are sorted by the chosen column too; see Sorting and Folder sizes.
- **Quick search only with Alt+letter** (`alt_search`, default off): Alt+character starts the
  quick search; while it runs, plain characters continue it. Without a running search plain
  characters do nothing (`\` and Space keep working).
- **Space marks without moving down** (`space_stays`, default off): Space toggles the mark (and
  counts a folder) but the cursor stays; Insert still moves down.
- Everything is saved in `%APPDATA%\RustCommander\config.json` (`RC_CONFIG_DIR` overrides the
  directory), written by a small JSON reader/writer of our own (no serde):

  ```json
  {
    "panels": {
      "left": "C:\\…", "right": "D:\\…", "active": 1,
      "left_sort": { "by": "size", "desc": true }, "right_sort": { "by": "name", "desc": false },
      "left_drives": { "C": "C:\\…", "D": "D:\\…" }, "right_drives": { … }
    },
    "window": { "left": 100, "top": 100, "right": 1500, "bottom": 900, "maximized": false },
    "mask": "*.rs",
    "lister": { "window": { … }, "wrap": true },
    "editor": "notepad.exe",
    "options": { "exact_sizes": false, "alt_search": false, "sort_dirs": false, "space_stays": false,
                 "command_line": false, "language": "en" },
    "pack": { "format": "zip", "level": "normal", "subdir": true }
  }
  ```

  Unknown keys are ignored, missing ones get their default, a broken file gives all defaults.
  An old `config.ini` is not read.

### Languages

- English and German. Every text lives in `i18n/en.json` / `i18n/de.json` (embedded at compile
  time), nested by area and looked up by dotted key (`i18n::t("dialog.copy")`). Placeholders are
  named (`{name}`), counts use `<key>_one` / `<key>_other` (`i18n::tn`). A missing text falls back
  to English, then to the key. A test checks that both files have the same keys.
- Numbers and dates follow the language: `1.2 GB`, `48,318,382,080`, `2026-10-15  09:21` (English,
  ISO date) / `1,2 GB`, `48.318.382.080`, `15.10.2026  09:21` (German). Key names in hints too
  (`Ctrl`/`Strg`, `Ins`/`Einfg`); the conflict keys are S/A/N in English, S/A/K in German.
- Until a language is chosen in the options, the Windows display language decides (German →
  German, anything else → English); `options.language` is written only once chosen. Switching
  redraws everything right away, lister windows included.
- Not translated: error texts that come from Windows, the shell context menu (both in the Windows
  language) and the window title.
- Adding a language: a new `i18n/<id>.json`, one entry in `i18n::Lang`.

### File search (Alt+F7)

- Dialog with three fields: **Name** (file mask syntax as for Num+; masks without `\` find files
  and folders, `name\` only folders; an excluded folder like `| target\` is not searched),
  **Suchen in** (preset with the active panel's folder; inside an archive: the folder holding it)
  and **Enthält Text** (optional; case-insensitive incl. umlauts, in UTF-8, ANSI and UTF-16 LE;
  folders never match).
- Runs in the background on one thread per core (2–16) sharing one stack of folders, reading
  directories like the folder sizes (`FindFirstFileExW`) and file contents memory-mapped with the
  lister's byte search. Hidden and system files are included; directory links are not followed;
  archives are not looked into.
- Hits appear live; the status line shows hits and searched folders. When done, the hits are
  sorted by path. Esc cancels a running search (the hits stay), then closes the dialog.
- Keys: Tab / ↑↓ between fields and the list, Enter in a field starts the search, Enter in the
  list goes to the hit (its folder in the active panel, cursor on it), F3 views it. Click selects,
  double click goes there, the wheel scrolls.
- The last search is kept: Alt+F7 shows it again with the list focused, so the next hit is
  Alt+F7, ↓, Enter.
- Later: results as a panel ("feed to listbox"), size/date filters, regular expressions, searching
  inside archives.

### Shell context menu (long right press, Shift+F10, menu key)

- Explorer's menu (incl. 7-Zip, "Öffnen mit", "Senden an" …) for the marked entries, else the
  entry under the cursor; on `..` for the directory itself. Not inside archives.
- Long right press: the menu opens at the mouse while the button is still held; releasing it
  chooses nothing. Keyboard: below the cursor row.
- "Umbenennen" opens the app's own rename dialog (the shell's only works inside Explorer); every
  other command is run by the shell. Changes show up through the directory watcher.
- The menu runs its own message loop, so it is opened from `window.rs` outside of `AppCell::with`;
  submenu messages are forwarded to `IContextMenu3`. The UI thread is OLE-initialized (copy/cut).

### File masks (Num+ / Num−)

`*` and `?`, case-insensitive; several masks separated by `;` or spaces (`"…"` keeps spaces);
`|` starts the excludes (`*.* | *.bak`). Masks apply to files only, unless they end in `\`
(`*\` = all directories). A mask without wildcards means "contains". `*.*` also matches names
without a dot. Num+ adds to the existing marks, Num− only removes matches. The main keyboard's
`+`/`-` stay quick search characters. The last mask is saved in `config.json`.

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

### Packing (Alt+F5)

- Packs the marked entries (else the one under the cursor; nothing on `..`) into a new archive,
  written with the same libarchive. Not inside archives.
- Dialog: the target (default `<other pane>\<name>.zip`; name = the single file's name without
  extension or the single folder's name, for several entries the current folder's name), **Format** zip / 7z (switching
  changes the extension in the target) and **Stufe** Speichern / Schnell / Normal / Maximal
  (level 0 / 1 / 6 / 9; zip uses Deflate, 7z LZMA2). Tab moves between input and rows, ←/→
  picks, a click picks too. Format and level are remembered (`"pack"` in `config.json`).
- The entries are at the archive's top level, folders with everything below them; file times
  are kept, names are stored as UTF-8 (zip) / UTF-16 (7z). An archive inside a packed folder
  leaves itself out.
- An existing target: the conflict question (overwrite replaces it, adding to an archive is not
  supported). Progress and cancel like copying; a cancelled or failed archive is deleted. A file
  that can't be opened is reported and left out. Afterwards the cursor is on the new archive.
- No passwords, no "move into archive".

### Unpacking (Alt+F9)

- Unpacks the marked archives (else the one under the cursor) completely; marked entries that
  aren't archives are skipped, with none at all a notice says so. Not inside archives (F5
  extracts there).
- Dialog: the target (default: the other pane's folder) and **Jedes Archiv in eigenen
  Unterordner** (default on, remembered): `fotos.zip` goes to `<target>\fotos\`.
- One job for all archives with progress, cancel and the conflict question; encrypted entries are
  reported and skipped. With one archive into its own folder the cursor lands on that folder.

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
- Window position and wrapping are saved in `config.json` (`lister.window`, `lister.wrap`).

## Deliberately later

Lister: selecting and copying text, images, following growing files, N/P for the next/previous
file, line numbers, hex search (`4D 5A`). Archives: adding to existing archives, password dialog, archives inside archives,
7-Zip's `7z.dll` as a fallback for Windows 10. Parallel copying of many small files, system icons (async, cached), live filter, light theme, normalizing path casing
(typed `c:\windows` → `C:\Windows`), partial redraws (if frame time on large windows becomes an issue).
