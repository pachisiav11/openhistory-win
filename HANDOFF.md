# Handoff — OpenHistory for Windows

Last updated 2026-08-23, after the launch-hang fix, the sign-in setting, the consent
fix, and the day's screen-time and search-to-hour work. Read this before touching
anything.

## Where the project stands

Every planned phase is complete, committed, tagged and pushed.

| Phase | State | Tag |
| --- | --- | --- |
| 0 — Repo, workspace, CI, docs | Done, pushed | `phase-0` |
| 1 — Native activity collector | Done, pushed | `phase-1` |
| 2 — Tauri shell writing JSONL | Done, pushed | `phase-2` |
| 3 — Processing layer | Done, pushed | `phase-3` |
| 4 — Inference layer | Done, pushed | `phase-4` |
| 5 — MCP server | Done, pushed | `phase-5` |
| 6 — React frontend | Done, pushed | `phase-6` |

Repository: <https://github.com/pachisiav11/openhistory-win> (public, account `pachisiav11`).
Git flow agreed with the user: one commit per phase on `main`, tagged `phase-N`, pushed.

## What each of the last three phases added

### Phase 4 — `crates/oh-inference`

- `provider.rs` — the `Provider` trait every backend implements, and the error type the
  interface renders.
- `anthropic.rs`, `openai.rs`, `google.rs` — one client each. All three are tested
  against a local HTTP server that speaks the vendor's response shape.
- `prompt.rs` — builds the request from `PublicEpisode` values only, so redaction is
  structural rather than a filter. See AD-14.
- `catalog.rs` / `catalog.json` — five local models, each verified against the live
  Hugging Face listing. See AD-5, and the warning below.
- `download.rs` — resumable GGUF download with progress events and cancellation.
- `llama.rs` — spawns `llama-server`, waits on `/health`, unloads after an idle timeout.
- `secrets.rs` — Windows Credential Manager, one entry per provider, write-only from the
  window's point of view. See AD-15.
- `service.rs` — the orchestration: readiness, per-hour and per-day runs, consent gate.

### Phase 5 — `crates/oh-mcp`

- `tokens.rs` — SHA-256 digests only, constant-time compare. A token is shown once. See
  AD-16.
- `history.rs` — the sanitized view, built from `PublicEpisode`.
- `server.rs` / `rpc.rs` — axum on `127.0.0.1` with port fallback; REST routes and a
  JSON-RPC `/mcp` endpoint over the same handlers. See AD-17.
- `src-tauri/src/mcp.rs` — `McpState::reconcile` restarts the server when the port or the
  history setting moved and leaves it alone otherwise.

### Phase 6 — the window

- `src/App.tsx` — five-view shell, status header, Pause/Resume, and a `revision` counter
  bumped 800 ms after the last status push. Views reload from that rather than each
  subscribing separately.
- `src/views/Timeline.tsx` — episodes grouped by the hour they started, never split
  across a boundary. Hour headings use the backend's proportional rollup.
- `src/views/Search.tsx` — 300 ms debounce, at most fifty hits.
- `src/views/DayView.tsx` — day navigation, hourly bars, top applications, whole-day and
  per-hour summaries. When a summary cannot be written the buttons stay visible and carry
  the backend's own reason.
- `src/views/Settings.tsx` — recording, the one-list model dropdown, per-provider keys,
  the local catalog with live progress, the MCP section, and deleting all history.
- `src/views/Summary.tsx` — the day composed into one Markdown document, and the library
  of days that were kept. Reads through `src/lib/markdown.ts`, which covers exactly the
  grammar `src-tauri/src/library.rs::compose` emits and nothing else. See AD-25.
- `src/lib/browser-mocks.ts` — the whole application runs in a plain browser, including a
  working in-memory library. Exporting is the one command that refuses there, because a
  browser tab has no save dialog and a pretended success would be a lie. See AD-18.

Backend additions Phase 6 needed: `EventStore::delete_all` (closes the open log first,
because Windows will not remove a file this process holds), `SummaryStore::clear`,
`Processor::forget_all`, and the `delete_all_history` command that stops the collector,
deletes, then resumes if it had been running.

## Test state at the stopping point

Everything below was run and passed:

- `cargo fmt --all -- --check` — clean.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test --workspace` — 297 passed, 0 failed, 10 ignored.
- `npm test` — 76 passed.
- `npm run build` and `npx tsc --noEmit` — clean.
- The golden path driven in the browser preview: choose a model, store a key, give
  consent, enable the MCP server and see a token once, read the client snippet, write a
  day's summary from the Day view.

Earlier phase gates still pass and still need a desktop:

```bash
cargo test -p oh-collector --test live_desktop -- --ignored --test-threads=1
```

```bash
cargo test -p openhistory-win --test persistence -- --ignored --test-threads=1
```

**Known flake, not a defect:** `LNK1104: cannot open file …exe` sometimes appears when
relinking a test binary immediately after running it. Re-run the command; it links.

**Second known flake:** `lets the user agree to cloud summaries before any model is
chosen` in `src/views/Settings.test.tsx` timed out twice in about twenty whole-suite runs
on a loaded machine, and never once in twenty-two runs afterwards, including ten of that
file alone. Its `waitFor` uses the 1000 ms default while five test files run in parallel.
Nothing in the test or the view depends on real time. Re-run it; if it ever fails twice
in a row, that is new and worth reading properly.

## Traps that cost time before

- **A test that reads the Windows Credential Manager passes or fails by whose machine it
  runs on.** `each_cloud_provider_names_its_own_missing_key` asserted that no key was
  stored, and fell through `secrets::load` to the real credential store to find out. It
  went red the moment a Google key was saved in the application. `secrets` now has
  `pretend_missing` alongside `pretend_stored`, and a test that expects a gap says so.
  Do not let the fall-through back in for anything but the two ignored round-trip tests.

- **Never read one of our own windows from the collector thread.** `GetWindowTextW` and
  `GetWindowTextLengthW` send a message to the thread that owns the window and wait for
  it to be pumped. Between processes Windows short-circuits that; inside one process it
  does not. The collector's opening snapshot did exactly this to the application's own
  window while the main thread sat inside `Collector::start` waiting for the collector,
  and the window was drawn as "Not responding" on every launch that put it in front. The
  WinEvent hooks were never exposed to it because they carry `WINEVENT_SKIPOWNPROCESS`.
  `describe` now refuses our own windows, and the collector releases its caller before
  taking the snapshot. See AD-19.

- **A UIAutomation walk is a cross-process call per step.** Reading the text a window is
  showing has to be bounded by breadth *and* depth, not just by how much text is kept, or
  a large document view costs more than the interval between window switches and the
  collector starts measuring its own latency. `uia::visible_text` stops at 80 elements,
  4 levels, 24 children per node, and reads a given window at most once every thirty
  seconds. See AD-24.
- **Redaction that only guards the network does not guard the disk.** `oh-collector/src/
  text.rs` runs before anything is written, not before anything is sent. It is
  deliberately eager — a twenty-character run of mixed letters and digits is dropped
  whether it is an API key or a genuine word — because the two failures are not
  symmetrical. A test documents the trade so nobody "fixes" it.
- **jsdom implements no scrolling at all.** `Element.prototype.scrollIntoView` is
  undefined there, so a view that scrolls a row into sight throws in the test suite for a
  reason that has nothing to do with the view. `src/test/setup.ts` stubs it.
- **A one-shot request passed as a prop has to be consumed.** The Day view is told which
  hour to scroll to. Held as ordinary state in the shell, clicking the same search result
  twice sets the value it already holds, React sees no change and the second click does
  nothing. The Day view calls `onFocused` to clear the request, and keeps the mark as its
  own state. See AD-23.
- The model catalog cannot be written from memory. The first version had three
  repositories that do not exist and two whose sizes were wrong by nearly half. Check
  every entry against the repository's own file listing. See AD-5.
- `winver.exe` reports its application name as **"Version Reporter Applet"**, and its
  first window title is the raw format string `About %s`. The live gate derives its
  search term from the recorded data for this reason — do not hardcode application names
  in tests.
- Windows 11 Notepad is a **packaged** application. Spawning `notepad.exe` and killing the
  returned child leaves the real window open on the user's desktop. The gate uses
  `charmap.exe`, a plain Win32 executable, instead. Same trap applies to `calc.exe` and
  probably `mspaint.exe`.
- The search index holds what was **displayed** (application name, window title, URL
  path), never what was launched. Executable paths are on the episode but not indexed.
- A `Processor` must not be constructed twice. The window and the MCP server share one
  behind a mutex; two would each hold their own in-memory search index.
- Settings must apply a change to the newest configuration, not the one the render closed
  over, or a second toggle made before the first round trip returns carries the first
  field back. There is a test for exactly this.

## Decisions already settled with the user — do not re-ask

- Architecture is my choice, optimised for quality, small download, and modest memory
  (explicitly "not like Electron"). Hence Rust + Tauri v2 + WebView2, no bundled JS runtime.
- Local inference: app-managed `llama-server`, spawned on demand, unloaded when idle, with
  in-app download and progress. Curated catalog plus a custom GGUF path.
- Summary model choice is **one dropdown**: three Anthropic (Haiku, Sonnet, Opus latest),
  three OpenAI (Luna, Terra, Sol), one Google (`gemini-flash-latest`), plus installed local
  models. **No preselected default.**
- Cloud providers: mock them in tests, flag the live call as an unverified gap. **Never ask
  the user to paste a secret into chat.**
- Privacy: nothing leaves the machine without an explicit opt-in.

## Standing constraints

From the user's global `CLAUDE.md`:

- **Ask before deleting any file.** This is the only operation needing confirmation.
- **Never skip git hooks.** `--no-verify` is forbidden.
- Never force-push to `main`.
- Do not commit unless the user asks.
- Keep `todo.md` and `TODO_log.md` current.
- ASD-STE100 in chat only, never in file content.
- Verify UI changes in a browser before calling them done.

## Environment notes

- **No computer-use is available for this project.**
- **Browser-pane screenshots do not work here.** Use `get_page_text`, `read_page`,
  `javascript_tool` and `read_console_messages` instead. Appearance is therefore
  unverified — only structure and content are.
- Editing files while `npm run dev` is running can leave Vite serving a stale module and
  reporting `does not provide an export named 'default'`. Restart the dev server.
- PowerShell here-string syntax (`@'…'@`) is not valid in the Bash tool. A commit message
  written that way keeps the `@` markers as message lines.

## Verification gaps carried forward

Recorded in `docs/ARCHITECTURE.md` AD-7, repeated here because they are easy to lose:

- Session lock and sleep are exercised with synthetic window messages, not a real lock.
- Private-browsing detection is verified live only on Chrome and Edge. Firefox, Brave,
  Opera, Vivaldi, and Arc are from documentation alone.
- Appearance is not verified — screenshots are unavailable in this environment.
- The tray icon is not driven by any test.
- No request has ever been sent to a real cloud provider.
- No GGUF has ever been downloaded, and `llama-server` has only ever been a stub.
- Nothing checks that the five catalog repositories still exist; that is a release step.
