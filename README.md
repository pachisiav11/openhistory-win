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
| 0 | Repository, workspace scaffold, CI | in progress |
| 1 | Native activity collector | pending |
| 2 | Tauri shell, JSONL persistence | pending |
| 3 | Episode detection, rollups, search index | pending |
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
- Password fields are never read; UIAutomation reports them and they are skipped.
- Password managers are excluded out of the box, and you can exclude any application.
- No activity data leaves the machine until you turn on a cloud provider and confirm it.
- Local summarization runs entirely offline once a model is downloaded.
- MCP responses expose summaries, never raw event streams.

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

## Documentation

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — design decisions and their rationale
- [`docs/CONSOLIDATED.md`](docs/CONSOLIDATED.md) — the original master plan and schema reference
- [`docs/tauri-llm-guide.md`](docs/tauri-llm-guide.md) — llama.cpp integration blueprint

## License

MIT. See [`LICENSE`](LICENSE).
