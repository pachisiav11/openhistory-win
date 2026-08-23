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

All seven phases have landed. See [`todo.md`](todo.md) for what is still open and
[`TODO_log.md`](TODO_log.md) for what has been finished.

| Phase | Scope | State |
|-------|-------|-------|
| 0 | Repository, workspace scaffold, CI | done |
| 1 | Native activity collector | done |
| 2 | Tauri shell, JSONL persistence | done |
| 3 | Episode detection, rollups, search index | done |
| 4 | Inference: three cloud providers and a managed llama.cpp | done |
| 5 | Local MCP server | done |
| 6 | React frontend | done |

Two things the automated suite cannot reach are written down rather than assumed: no
request has ever been sent to a real cloud provider, and no GGUF has ever been
downloaded or run. Both paths are covered against local servers that speak the same
shapes. `docs/ARCHITECTURE.md` (AD-7) lists every such gap.

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
        +--> oh-inference (Rust)  Anthropic | OpenAI | Google  or  llama-server
        +--> oh-mcp       (Rust)  127.0.0.1, bearer auth, REST and JSON-RPC
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
summaries/2026-08-21.json      the hours and the day, written by a model
models/                        downloaded GGUF files
tokens.json                    SHA-256 digests of issued MCP tokens, never the tokens
```

API keys are not in this tree. They go to the Windows Credential Manager, one entry
per provider, and the app can ask whether a key exists but cannot read one back.

Event logs are cut on your local date and never rewritten, so a crash can cost at most
the event being written. Nothing is compressed or encoded — `type` and `jq` both work.
Set `OPENHISTORY_DATA_DIR` to put the whole tree somewhere else.

Everything outside `events/` is derived from it. Episodes group consecutive activity in
one application; each one records the time it spanned and, separately, the time there is
evidence for — a window left in the foreground overnight is not eight hours of work.
Delete `episodes/` and `index/` and they are rebuilt from the event log.

## Reading a day

The Day view ranks the day's applications by time spent and gives the day two figures:
the time at the machine, and how much of that was working time. The difference between
them is idle — a window in front with nothing happening — and it is listed as its own
row rather than credited to whatever application happened to be open. Time while the
screen was locked or asleep is in neither figure; being away is not being idle.

A search result opens the day it came from at the hour it happened in, with that hour
marked, so a match found in a fortnight of history is one click from its context.

Closing the window leaves the app recording in the tray. Quit from the tray menu to
stop it entirely.

Windows starts the app when you sign in, so a restart does not leave a hole in the day.
Started that way it goes straight to the tray without opening a window; started by you
it opens normally. Turn it off in Settings, which removes the app's entry under
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` — nothing is written outside
your own user.

## Summaries

Summaries are off until you choose a model. Settings offers one list, grouped by who
runs it: three Anthropic models, three OpenAI, one Google, and any local model you have
downloaded. Picking a model is what sets the provider — there is no second dropdown to
keep in step.

A cloud model needs two more things before anything is sent: that provider's API key,
and your explicit agreement. The agreement text names exactly what goes out. A private
session is reduced to an application and a span of time, a URL loses its query string,
and no file path is ever included. The redaction is a type conversion rather than a
filter, so there is no code path that could send a private title by accident.

You can write a whole day or a single hour, rewrite either, or forget a day's summaries
entirely. Each summary records which model wrote it.

### Local models

Local inference downloads nothing until you pick a model. When a summary is due the app
starts `llama-server`, generates, then shuts it down after an idle timeout, so the
resting memory cost is zero.

The curated catalog is tuned for laptop-class hardware. Every row was read off the
repository's own file listing rather than estimated:

| Model | File | Size |
|-------|------|------|
| Qwen3 1.7B | `Qwen/Qwen3-1.7B-GGUF` | 1.83 GB |
| Phi-3 mini 4k | `microsoft/Phi-3-mini-4k-instruct-gguf` | 2.39 GB |
| Qwen3 4B | `Qwen/Qwen3-4B-GGUF` | 2.5 GB |
| Gemma 4 E2B QAT | `google/gemma-4-E2B-it-qat-q4_0-gguf` | 3.35 GB |
| Gemma 4 E4B QAT | `google/gemma-4-E4B-it-qat-q4_0-gguf` | 5.15 GB |

Settings measures each against the machine's RAM and says which do not fit.

## Answering other programs

An optional MCP server lets another program on this machine — Claude Code, an editor
extension, a shell script — ask what you have been working on. It is off by default. It
binds `127.0.0.1` only, and every request needs a bearer token issued from Settings.

The token is shown once. Only its SHA-256 digest is stored, so it cannot be shown again;
regenerating one invalidates every earlier token. Settings prints the whole client
configuration snippet at the moment the token is issued, ready to paste.

The server answers over plain REST for a script and over JSON-RPC at `/mcp` for an MCP
client, through the same handlers. Every response is built from the same redacted view
the cloud providers see: summaries and episode shapes, never a raw event stream.

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
npx tauri build
```

That produces `target/release/openhistory-win.exe` (about 9 MB) and an NSIS installer at
`target/release/bundle/nsis/` (about 3 MB). There is no bundled runtime in either.

## Testing

```bash
cargo test --workspace
```

```bash
npm test
```

The frontend runs outside Tauri against a mocked IPC layer, so `npm run dev` opens a
working application in an ordinary browser and the view tests need no desktop session.
The mocks hold the backend's invariants — a private episode has no title, a stored key
never comes back, a token is shown once — so a view cannot be written against data the
real backend would never produce.

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
