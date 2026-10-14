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
- `unsafe` lives only in the OS boundary modules (`win.rs`, `gfx.rs`, `fsx.rs`, `main.rs`,
  `timing.rs`). App logic (`app.rs`, `panel.rs`) is free of `unsafe`. The window procedure guards
  the app against reentrant access (`AppCell`).

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
| Enter / double-click | Open directory, or open file via shell |
| Backspace, Ctrl+PgUp | Parent directory |
| \ | Drive root (also works on German keyboard layouts) |
| Alt+F1 / Alt+F2 | Choose drive for left / right pane |
| Ctrl+L, click on path | Edit path (Ctrl+V, environment variables) |
| Ctrl+R, F2 | Reload |
| Type letters | Quick search (jumps to first match), Esc cancels |

On startup the last directories and the window position are restored
(`%APPDATA%\RustCommander\config.ini`).

## Deliberately later

File operations (copy/move/delete) + selection, automatic refresh (`ReadDirectoryChangesW`),
system icons (async, cached), live filter, light theme, settings, normalizing path casing
(typed `c:\windows` → `C:\Windows`), partial redraws (if frame time on large windows becomes an issue).
