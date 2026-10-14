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
| `main.rs` | Startup: arguments, config, window, message loop. |
| `window.rs` | Window class, `wndproc` (routes every message to an `App` method), `AppCell`. |
| `app/` | The application state and everything that changes it. |
| `app/mod.rs` | `App` struct, `Mode` (normal / dialog / popup …), timers, directory load results. |
| `app/keyboard.rs`, `app/mouse.rs` | Input handling. |
| `app/dialogs.rs` | Dialogs, drive popup, path editing, mark by mask. |
| `app/jobs.rs` | Starting file operations and handling their progress, conflicts and results. |
| `app/layout.rs` | Sizes (DIPs), panel geometry, hit-testing, scrollbar geometry. |
| `app/text_input.rs` | Single-line text field. |
| `app/format.rs` | Pure helpers: size/date formatting, name checks, path parsing. |
| `ui/` | Drawing only – reads `App`, never changes it. `paint()` redraws the whole window. |
| `panel.rs` | One file list: directory, entries, cursor, scroll position, marks, quick search. |
| `files/` | Files, but not operations on them: listing directories (`listing`), UTF-16 names and natural sort (`names`), marking masks (`mask`), change notifications (`watch`). |
| `ops/` | File operations on a worker thread: `copy`, `moving`, `delete`; `mod.rs` holds the public API (`Job`, `Handle`, `Event`) and the shared worker logic. |
| `sys/` | Thin safe wrappers around Win32 (`win`) and Direct2D/DirectWrite (`gfx`). |
| `config.rs`, `theme.rs`, `timing.rs` | Settings file, colors, `--timing` measurements. |

`App` is one struct, but its methods are spread over the files in `app/` and `ui/` (one
`impl App` block per file), grouped by topic.

## Threads and messages

| Thread | Started by | Reports with | Handled in |
|---|---|---|---|
| Directory loader (one per load) | `Panel::navigate` / `reload` | `WM_APP_LOADED` + channel | `App::on_loaded` → `Panel::apply` |
| File operation worker | `App::start_job` → `ops::start` | `WM_APP_JOB` + channel | `App::on_job_event` |
| Directory watcher (one per panel) | `App::on_loaded` | `WM_APP_DIRCHANGED` (wParam = panel) | `App::on_dir_changed` → throttled reload |
| Shell open | `win::shell_open` | – | – |

Data always travels through an `mpsc` channel; the posted message only wakes the UI thread.
A loader result that arrives after a newer load was started is dropped (`Applied::Stale`).
The worker asks about conflicts by sending `Event::Conflict` and blocks until the UI answers
through `Handle::answer`.

Timers (`WM_TIMER`, IDs in `app/mod.rs`) are all one-shot: "loading" indicator, throttled
refresh, notice timeout, delayed progress card, drag auto-repeat.

## Life of a key press

`WM_KEYDOWN` → `wndproc` → `App::on_key` → depends on `App::mode`: a dialog, the path editor, the
drive popup, or `on_key_normal` (cursor, marking and command keys). The handler changes state
and calls `invalidate()`; the next `WM_PAINT` draws everything anew (`ui/mod.rs`). Only visible
rows are drawn, which keeps a frame at a few milliseconds.

## Rules

- **`unsafe`** only in the OS boundary: `main.rs`, `window.rs`, `sys/`, `files/listing.rs`,
  `files/watch.rs`, `ops/`, `timing.rs`. `app/`, `ui/` and `panel.rs` call safe wrappers.
- **Reentrancy:** Win32 sometimes calls `wndproc` again while a handler is still running.
  `AppCell` detects that instead of creating a second `&mut App`. Calls that send messages
  synchronously (`SetWindowPos`, `DestroyWindow`, `SetCapture`, …) are made outside of
  `AppCell::with`.
- **Units:** layout in DIPs (1/96 inch); `sys::gfx` scales to pixels.
- **Names** are UTF-16 (`Box<[u16]>`) as Windows delivers them; no conversion per entry.
- **Formatting:** `cargo fmt` (`rustfmt.toml`, width 120). Code, comments and docs in English;
  the UI text is German for now.
- **Tests:** `cargo test`. File operation tests run against throw-away directories in `%TEMP%`.
