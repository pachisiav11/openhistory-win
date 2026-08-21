# OpenHistory for Windows

A local-first activity history for Windows 11. It records what you worked on — the
foreground application, the window title, the browser URL, and session events like
lock and wake — groups that raw stream into coherent episodes, and turns each hour
and each day into a short written summary you can read or query.

Everything stays on your machine unless you explicitly turn on a cloud provider.

This is a ground-up Windows port of the original macOS OpenHistory. The recorded
`ActivityEvent` JSON schema is byte-identical to the original, so history files are
interchangeable between the two.

## Status

Under active construction, built in phases. See [`todo.md`](todo.md) for what is in
flight and [`TODO_log.md`](TODO_log.md) for what has landed.

| Phase | Scope | State |
|-------|-------|-------|
| 0 | Repository, workspace scaffold, CI | done |
| 1 | Native activity collector | done |
| 2 | Tauri shell, JSONL persistence | done |
| 3 | Episode detection, rollups, search index | done |
| 4 | Inference: Anthropic and local llama.cpp | pending |
| 5 | Local MCP server | pending |
| 6 | React frontend | pending |

## Design

The whole backend is Rust. The collector talks to Win32 and UIAutomation through the
`windows` crate; processing, inference orchestration and the MCP server are Rust
modules in the same binary. The interface is React and TypeScript rendered in
WebView2, which ships with Windows 11.

There is no bundled JavaScript runtime and no second process at rest. That is a
deliberate departure from the original design, which used a C++ N-API addon loaded by
a Node sidecar; the reasoning is written up in
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

```
Win32 WinEventHook + UIAutomation + power/session notifications
        |
        v
  oh-collector  (Rust)  normalizes to ActivityEvent JSON
        |
        v
  oh-core       (Rust)  append-only JSONL in %APPDATA%\openhistory-win
        |
        v
  oh-processing (Rust)  episodes, hourly/daily rollups, inverted search index
        |
        +--> oh-inference (Rust)  Anthropic API  |  managed llama-server
        +--> oh-mcp       (Rust)  127.0.0.1:47123, bearer auth
        +--> React UI     (WebView2)  timeline, search, day view, settings
```

## Privacy

The app reads window titles and browser URLs, so it treats that data carefully by
default.

- Private and incognito browser windows emit a `privacyBoundary` event and nothing else.
  Detection reads the accessibility tree, not the window title, because current Chrome
  no longer marks an incognito window in its title at all. A browser window the app
  cannot inspect is treated as private rather than assumed safe.
- Password fields are never read; UIAutomation reports them and they are skipped.
- Password managers are excluded out of the box, and you can exclude any application.
- No activity data leaves the machine until you turn on a cloud provider and confirm it.
- Local summarization runs entirely offline once a model is downloaded.
- MCP responses expose summaries, never raw event streams.

## Where your history lives

Everything is plain text under `%APPDATA%\openhistory-win`:

```
config.json                    settings, safe to edit by hand
events/2026-08-21.jsonl        one JSON object per line, append-only
episodes/2026-08-21.json       that day grouped into episodes, with totals
index/search-index.json        inverted index over every episode
summaries/                     hourly and daily writing  (phase 4)
models/                        downloaded GGUF files     (phase 4)
```

Event logs are cut on your local date and never rewritten, so a crash can cost at most
the event being written. Nothing is compressed or encoded — `type` and `jq` both work.
Set `OPENHISTORY_DATA_DIR` to put the whole tree somewhere else.

Everything outside `events/` is derived from it. Episodes group consecutive activity in
one application; each one records the time it spanned and, separately, the time there is
evidence for — a window left in the foreground overnight is not eight hours of work.
Delete `episodes/` and `index/` and they are rebuilt from the event log.

Closing the window leaves the app recording in the tray. Quit from the tray menu to
stop it entirely.

## Local models

Local inference is opt-in and downloads nothing until you pick a model. When a summary
is due the app starts `llama-server`, generates, then shuts it down after an idle
timeout, so the resting memory cost is zero.

The curated catalog is tuned for laptop-class hardware:

| Model | Approx. Q4 size | Notes |
|-------|-----------------|-------|
| Gemma 4 E2B QAT, text-only | ~0.9 GB | Official Google QAT weights. Fastest option. |
| Gemma 4 E4B QAT, text-only | ~3–5 GB | Native structured JSON output, 128K context. |
| Qwen3.5-4B | ~2.5–3.4 GB | Strong at reading structured input. |
| Phi-4-mini 3.8B | ~2.3–3 GB | Best reasoning density in its size band. |
| Qwen3.5-2B | ~1.3 GB | Light second option for battery use. |

Any other GGUF works too — point Settings at the file.

## Building

Prerequisites:

- Rust stable, MSVC toolchain
- Node.js 20 or newer
- Windows SDK 10.0.22000 or newer
- Tauri CLI v2: `cargo install tauri-cli --version "^2"`

```bash
npm install
cargo tauri dev
```

Release build:

```bash
cargo tauri build
```

## Testing

```bash
cargo test --workspace
```

```bash
npm test
```

Some tests are `#[ignore]`d because they open real windows and need an interactive
desktop — including the one that launches every installed browser in private mode and
asserts nothing about it is recorded, the one that records a real session and reads it
back off disk, and the one that records two applications in turn and checks that they
become searchable episodes with measurable time in them. Run those deliberately:

```bash
cargo test -p oh-collector --test live_desktop -- --ignored --test-threads=1
```

```bash
cargo test -p openhistory-win --test persistence -- --ignored --test-threads=1
```

## Documentation

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — design decisions and their rationale
- [`docs/CONSOLIDATED.md`](docs/CONSOLIDATED.md) — the original master plan and schema reference
- [`docs/tauri-llm-guide.md`](docs/tauri-llm-guide.md) — llama.cpp integration blueprint

## License

MIT. See [`LICENSE`](LICENSE).
