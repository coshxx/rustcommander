# RustCommander – Architecture

A map of the code for humans. What the program does and why is in [SPEC.md](SPEC.md).

## Big picture

One Win32 window, everything drawn with Direct2D. The UI thread never waits for the disk:
directory listings, file operations and change notifications run on their own threads and
report back by posting window messages. All state lives in one `App` value that only the UI
thread touches.

```
           ┌──────────── UI thread ────────────┐
 Windows ──► window.rs (wndproc) ──► App (app/) ──► ui/ (draw)
 messages  └─────▲──────────────▲──────────────┘
                 │ WM_APP_*     │ WM_APP_*
     loader threads       worker thread       watcher threads
     (files/listing)      (ops/)              (files/watch)
```

## Source tree

| Path | What it is |
|---|---|
| `build.rs`, `res/` | Embeds the icon into the exe (`rc.exe`); `make-icon.ps1` draws `app.ico`. |
| `main.rs` | Startup: arguments, config, window, message loop. |
| `window.rs` | Window class, title, icon, `wndproc` (routes every message to an `App` method), `AppCell`. |
| `app/` | The application state and everything that changes it. |
| `app/mod.rs` | `App` struct, `Mode` (normal / dialog / popup …), timers, directory load results. |
| `app/keyboard.rs`, `app/mouse.rs` | Input handling. |
| `app/dialogs.rs` | Dialogs, drive popup, path editing, mark by mask. |
| `app/find.rs` | Search dialog (Alt+F7): fields, hits, keys; the last search is kept in `App::find`. |
| `app/options.rs` | Options dialog (Ctrl+,): the list of switches; toggling saves the config. |
| `app/jobs.rs` | Starting file operations and handling their progress, conflicts and results. |
| `app/layout.rs` | Sizes (DIPs), panel geometry, hit-testing, scrollbar geometry. |
| `app/text_input.rs` | Single-line text field with a selection. |
| `app/rename.rs` | Inline renaming (slow second click on a name). |
| `app/format.rs` | Pure helpers: size/date formatting, name checks, path parsing. |
| `ui/` | Drawing only – reads `App`, never changes it. `paint()` redraws the whole window. |
| `panel.rs` | One file list: directory, entries, cursor, scroll position, marks, quick search. |
| `files/` | Files, but not operations on them: listing directories (`listing`), file search (`find`), folder sizes and the shared directory reader `scan` (`dirsize`), sort order (`sort`, pure and tested), archives as read-only directories (`archive`), UTF-16 names and natural sort (`names`), marking masks (`mask`), change notifications (`watch`), temp files for opening entries of archives (`temp`). |
| `ops/` | File operations on a worker thread: `copy`, `moving`, `delete`, `extract` (from archives); `mod.rs` holds the public API (`Job`, `Handle`, `Event`) and the shared worker logic. |
| `lister/` | The F3 viewer: its own windows (`window`), state and commands (`mod.rs`), input, drawing, the byte → screen line logic (`text`, pure and tested) and the search (`search`). |
| `sys/` | Thin safe wrappers around Win32 (`win`), Direct2D/DirectWrite (`gfx`), libarchive from Windows' `archiveint.dll` (`libarchive`), memory-mapped files (`mapped`) and the shell context menu (`shell_menu`). |
| `config/` | Settings file `config.json` (`mod.rs`, incl. `Options`) and a minimal JSON parser/writer (`json`). |
| `i18n.rs`, `../i18n/*.json` | Translations: current language, `t` / `tf` / `tn` lookups; texts embedded from the JSON files. |
| `theme.rs`, `timing.rs` | Colors, `--timing` measurements. |

`App` is one struct, but its methods are spread over the files in `app/` and `ui/` (one
`impl App` block per file), grouped by topic.

## Threads and messages

| Thread | Started by | Reports with | Handled in |
|---|---|---|---|
| Directory loader (one per load) | `Panel::navigate` / `reload` | `WM_APP_LOADED` + channel | `App::on_loaded` → `Panel::apply` |
| File operation worker | `App::start_job` → `ops::start` | `WM_APP_JOB` + channel | `App::on_job_event` |
| Directory watcher (one per panel) | `App::on_loaded` | `WM_APP_DIRCHANGED` (wParam = panel) | `App::on_dir_changed` → throttled reload |
| File search workers (one set per search) | `App::start_find` → `find::start` | `WM_APP_FIND` (at most one pending) + channel of hit batches; `TIMER_FIND` repaints the folder count | `App::on_find_event` |
| Folder size workers (a pool, started on first use) | `Panel::count_size` → `dirsize::start` | `WM_APP_SIZE` when a count is done; live numbers are atomics, repainted by `TIMER_SIZES` | `App::on_size_done` |
| Shell open | `win::shell_open` | – | – |
| Temp cleanup (once) | `main` (`files::temp::sweep_old`) | – | – |
| Lister search (one per search) | `Lister::search` → `search::start` | `WM_APP_SEARCH` (to the lister window) + channel | `Lister::on_search_event` |

Data always travels through an `mpsc` channel; the posted message only wakes the UI thread.
A loader result that arrives after a newer load was started is dropped (`Applied::Stale`).
The worker asks about conflicts by sending `Event::Conflict` and blocks until the UI answers
through `Handle::answer`.

Timers (`WM_TIMER`, IDs in `app/mod.rs`) are all one-shot: "loading" indicator, throttled
refresh, notice timeout, delayed progress card, drag auto-repeat.

## Archives

A path like `C:\x\fotos.zip\urlaub` is an ordinary panel path. `files::listing::load` notices
the archive part (`files::archive::split`) and lists from the archive's table of contents
instead, which `files::archive::index` reads once with libarchive and caches. The `Listing`
carries the archive path; a panel with `archive` set is read-only, so `app/dialogs.rs` turns F5
into `Job::Extract` and refuses everything that would write. Extraction is one sequential pass
over the archive (`ops/extract.rs`); entry paths go through `archive::clean_path`, which rejects
anything that could leave the destination.

## Lister windows

F3 doesn't create the window right away: `App::request_view` posts `WM_APP_VIEW` to the main
window, and `window.rs` opens the lister outside of `AppCell::with` (creating a window sends
messages to the main window synchronously). Each lister window owns a `Lister` in its own
`AppCell<Lister>`, freed on `WM_NCDESTROY`; all of them run on the UI thread. The file is
memory-mapped; the view is a byte offset (`top`), and `lister/text.rs` turns offsets into screen
lines on demand (`line`, `back`, `align`), so nothing ever walks the whole file except a search.

## Life of a key press

`WM_KEYDOWN` → `wndproc` → `App::on_key` → depends on `App::mode`: a dialog, the path editor, the
drive popup, or `on_key_normal` (cursor, marking and command keys). The handler changes state
and calls `invalidate()`; the next `WM_PAINT` draws everything anew (`ui/mod.rs`). Only visible
rows are drawn, which keeps a frame at a few milliseconds.

## Rules

- **`unsafe`** only in the OS boundary: `main.rs`, `window.rs`, `sys/`, `files/listing.rs`,
  `files/dirsize.rs`, `files/watch.rs`, `ops/`, `timing.rs`, `lister/window.rs`. `app/`, `ui/` and `panel.rs` call safe wrappers.
- **Reentrancy:** Win32 sometimes calls `wndproc` again while a handler is still running.
  `AppCell` detects that instead of creating a second `&mut App`. Calls that send messages
  synchronously (`SetWindowPos`, `DestroyWindow`, `SetCapture`, …) are made outside of
  `AppCell::with`.
- **Units:** layout in DIPs (1/96 inch); `sys::gfx` scales to pixels.
- **Names** are UTF-16 (`Box<[u16]>`) as Windows delivers them; no conversion per entry.
- **Formatting:** `cargo fmt` (`rustfmt.toml`, width 120). Code, comments and docs in English;
  the UI text is German for now.
- **Tests:** `cargo test`. File operation tests run against throw-away directories in `%TEMP%`.
