# RustCommander – Spezifikation

Zwei-Fenster-Dateimanager für Windows. Oberstes Ziel: **rotzeschnell** (Startup und Dateioperationen).

## Leistungsziele

| Messpunkt | Ziel |
|---|---|
| Kaltstart bis erstes gezeichnetes Fenster mit Inhalt | < 100 ms |
| Ordner mit 10.000 Einträgen laden + anzeigen | < 50 ms |
| UI-Thread blockiert | nie (auch nicht bei Netzlaufwerken) |

Messen: `rustcommander --timing` (loggt nach stderr und `%APPDATA%\RustCommander\timing.log`),
`rustcommander --bench` (wie `--timing`, beendet sich nach dem ersten Frame mit Inhalt).

## Technik

- Nur Windows. Win32-Fenster + Direct2D/DirectWrite, komplett selbst gezeichnet.
- Einzige Abhängigkeit: `windows`-Crate. Release: LTO, `panic=abort`, `codegen-units=1`.
- Ordner lesen: `FindFirstFileExW` mit `FindExInfoBasic` + `FIND_FIRST_EX_LARGE_FETCH`,
  immer im Hintergrund-Thread, abbrechbar. Ladeanzeige erst nach 150 ms.
- Nur sichtbare Zeilen werden gezeichnet.
- **Direct2D im Software-Modus** (Standard). Gemessen: Hardware-Target kostet beim Start ~150 ms
  (D3D + GPU-Treiber laden) und zeichnet nicht schneller. `RC_HARDWARE=1` schaltet zum Vergleich um.

### Messwerte v0.1 (Release, 120 dpi)

| | Ergebnis |
|---|---|
| Kaltstart bis Frame mit Inhalt | ~40–45 ms (Hardware-Target: ~175 ms) |
| Ordner 10.000 Einträge laden + sortieren | 6–8 ms |
| `C:\Windows\System32` (5.000 Einträge) | ~2 ms |
| Frame-Zeit 1186×722 DIP / 2752×1081 DIP | ~7 ms / ~12,5 ms |

## Optik

- Eigenes Dark Theme; alle Farben zentral in `theme.rs` (Light kommt später).
- Normale Windows-Titelleiste, dunkel eingefärbt (DWM).
- Schrift Segoe UI Variable; Icons als Glyphen aus Segoe Fluent Icons, Farbe nach Dateityp.
- Spalten: Name (inkl. Endung) | Größe | Datum. (Später einstellbar: Endung als eigene Spalte.)
- Sortierung: `..` zuerst, Ordner vor Dateien, natürlich & case-insensitiv.
- Versteckte/System-Dateien: gedimmt.

## Bedienung (v0.1)

| Taste | Aktion |
|---|---|
| Tab | Seite wechseln |
| ↑ ↓ Bild↑ Bild↓ Pos1 Ende | Cursor |
| Enter / Doppelklick | Ordner öffnen bzw. Datei per Shell öffnen |
| Backspace, Strg+Bild↑ | Ordner hoch |
| \ | Laufwerkswurzel (funktioniert auch mit deutschem Layout) |
| Alt+F1 / Alt+F2 | Laufwerk links / rechts wählen |
| Strg+L, Klick auf Pfad | Pfad eingeben (Strg+V, Umgebungsvariablen) |
| Strg+R, F2 | Neu laden |
| Buchstaben tippen | Schnellsuche (springt zum ersten Treffer), Esc bricht ab |

Beim Start werden die zuletzt offenen Ordner und die Fensterposition wiederhergestellt
(`%APPDATA%\RustCommander\config.ini`).

## Bewusst später

Dateioperationen (Kopieren/Verschieben/Löschen) + Markieren, automatisches Neuladen
(`ReadDirectoryChangesW`), System-Icons (asynchron, gecacht), Live-Filter, Light Theme,
Einstellungen, Pfad-Schreibweise normalisieren (getippt `c:\windows` → `C:\Windows`),
nur geänderte Bereiche neu zeichnen (falls Frame-Zeit bei großen Fenstern stört).
