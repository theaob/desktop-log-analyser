# Log Analyser

A cross-platform desktop app for exploring a folder of log files the way Grafana Explore works with Loki: pick a folder, filter with a LogQL-style query or the label sidebar, see the volume histogram stacked by level, and drill into lines.

log4j and Logback logs are the first-class format: paste your `PatternLayout` and the app compiles it into a parser, groups stack traces into one event, reads rolled and gzipped files, and turns logger, thread, MDC keys and exception classes into filterable labels.

Built with Tauri 2, a Rust core (`crates/logcore`) and a React + TypeScript UI. Windows and Linux are the MVP targets; macOS comes later.

## Status

MVP in progress. Working today:

- Open a folder (recursive, include/exclude globs, `.gz`, UTF-8/UTF-16, CRLF) with a preview of the detected format and parsed sample lines per rolled-file group.
- log4j 1.x/2.x and Logback `PatternLayout` compiler (format modifiers, `%X{key}` MDC, `%highlight`/`%clr` wrappers, `${PROP:-default}`, named `%d` formats and time zones), plus built-in common patterns and Spring Boot's default.
- JSON layouts (JsonTemplateLayout, JSONLayout compact, ECS/Logstash) and a plain-text fallback with timestamp and level detection.
- Stack traces grouped into one event; the exception class becomes the `exception` label.
- Parallel indexing into LZ4-compressed chunks stored in the app data folder; reopening an unchanged folder is instant.
- Query language: stream selector, line filters (`|=`, `!=`, `|~`, `!~`, with `or`), `json` / `logfmt` / `regexp` parsers, field filters with numbers, durations and byte sizes, `and` / `or`.
- Level-stacked histogram with drag-to-zoom, time presets anchored to the newest log line, absolute ranges.
- Virtualized log list with paging, expandable rows, matched-term highlighting and click-to-filter on any field.
- Label sidebar with value counts, query editor with highlighting, autocomplete and error underlines, and a basic visual builder kept in sync with the text.
- Export of all matching lines to CSV or JSON lines.

Next increments: Tantivy full-text index for sub-second queries on 5–10 GB folders, XML layout, syslog and logfmt auto-detection, query history and saved queries, show context, and live tail (phase 2).

## Query cheat sheet

```logql
{level="error", source="payments/app.log"}
{logger=~"com\\.acme\\.payment.*", requestId="req-00043"}
{exception="java.net.SocketTimeoutException"} |= "Read timed out"
{} |~ "duration=4[0-9]{3}ms" != "healthcheck"
{source="gateway/app.log"} | json | status >= 500 and duration > 2s
{} | logfmt | took > 100ms
{} | regexp "took (?P<ms>\\d+)ms" | ms > 300
```

Labels: `level`, `source` (rolled-file group such as `app.log`), `file` prune whole chunks; `logger`, `thread`, `exception` and MDC/JSON keys are checked per event. Label regexes are fully anchored, as in Loki.

## Development

Prerequisites: Rust (stable), Node 22, and the Tauri system dependencies.

- Linux: `sudo apt install libwebkit2gtk-4.1-dev librsvg2-dev libayatana-appindicator3-dev`
- Windows: WebView2 (preinstalled on Windows 10/11) and the MSVC build tools.

```sh
npm install
npm run tauri dev           # run the app with hot reload
npm run tauri build         # installers: MSI/NSIS on Windows, AppImage/deb/rpm on Linux
cargo test -p logcore       # core tests
```

The app also accepts a folder on the command line: `log-analyser /var/log/myapp`.

### Test data and benchmarking without the UI

```sh
python3 scripts/gen_log4j_logs.py --out testdata/generated --size-mb 200
cargo run --release -p logcore --bin logq -- testdata/generated --preview
cargo run --release -p logcore --bin logq -- testdata/generated '{level="error"} |= "Timeout"' \
  --pattern '%d{yyyy-MM-dd HH:mm:ss.SSS} [%t] %-5level %logger{36} [%X{requestId}] - %msg%n'
```

## Layout

```
crates/logcore/     Rust engine: discovery, readers, log4j/JSON/plain parsing, chunk store, query language, search, export
  src/log4j/        PatternLayout compiler and common patterns
  src/query/        LogQL-subset AST, parser (with error spans) and evaluator
  src/bin/logq.rs   CLI front end for benchmarking
src-tauri/          Tauri 2 shell exposing the engine as commands
src/                React UI (Explore-style layout)
scripts/            Test-data generator
```

Workspace data (index and settings) lives in the OS app-data folder, keyed by the log folder's path, never inside the log folder.
