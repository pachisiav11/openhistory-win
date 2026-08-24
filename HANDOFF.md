# Handoff — OpenHistory for Windows

Last updated 2026-08-24, after the document-label fix, the Chromium capture fix and the
termination race. Read this before touching anything, and read the next section first.

## Stopping point, 2026-08-24

`main` carries the five items the previous session left open, four of them resolved and
one deliberately not. Everything below was run and passed on this tree:
`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` (298 passed, 11 ignored), `npx tsc --noEmit`, `npm test`
(76 passed), `npm run build`, and both ignored desktop gates — `live_desktop` (now 6) and
`persistence` (3).

**What changed.**

1. **A document named after its editor is no longer recorded.**
   `DocumentObservation::names_the_window` takes a path outright and otherwise requires
   the trimmed title to be at least three characters and to appear inside the window
   title, compared without case. `Collector::read_window` takes the title and drops a
   document that fails it. Notepad wrote `"document":{"title":"Text editor"}` before this.
2. **The visible-text read was never a budget problem, and the budget is unchanged at
   120 ms.** It was measured: 120 ms, 250 ms and 2 s all returned one line from a Chrome
   window, so time was not the constraint. The depth cap counted every level of the tree,
   and Chromium hangs its page ten or eleven levels down behind a spine of seven unnamed
   `Pane` elements — the walk stopped four levels into an empty corridor. The walk now
   charges only levels that named something, and a read that comes back with nothing but
   the window's own name no longer starts the thirty-second clock. Verified live: a Claude
   (Electron) window went from `["Claude"]` to twelve lines, and `winver` now reads its
   actual dialog text. See AD-24.
3. **An application that exits while handing the foreground on is now reported.** The
   collector parks the departing application in one slot so the two-second liveness timer
   re-asks about it. This race pre-dated the session and had been passing by luck; the
   deeper read in item 2 shifted the timing and it began failing two runs in three. See
   AD-27.
4. `HANDOFF.md` and `docs/ARCHITECTURE.md` are committed.
5. `todo.md` is empty by design. Everything is in `TODO_log.md`.

**Still open, and the reason.**

- **The commit message of `6e52875` still has a stray `@` as its first line**, so the real
  subject is on line 2. Fixing it means `commit --amend` and a force-push to `main`, which
  the standing constraints say needs explicit instruction. The user has been told and has
  not asked for it. **Use `git commit -F -` with a bash heredoc, or the PowerShell tool,
  but never mix the two.**
- **Nothing ships `llama-server`, and local inference cannot work without it.** The user
  now points at one in Settings and the path is stored as `inference.local_server_path`.
  Bundling the binary, or offering to download a llama.cpp release the way the model
  catalog already downloads GGUFs, is the obvious next step and was not taken here.
- **The visible-text read is dominated by window furniture.** A Chrome window now yields
  `["… - Google Chrome", "Minimize", "Maximize", "Close", "New Tab", "Back", "Forward",
  "Reload", "You", "Chrome", "Tab search", "File"]` — twelve lines, of which perhaps four
  are worth summarizing, and the page's own text is not among them. Breadth-first order
  reaches the frame before the content. Worth trying: skip control types that are pure
  frame, or prefer `Text` and `Document` elements when the line budget is contended. The
  browser gate will show the result immediately.
- **A browser page's text is still read before the page has loaded.** The read happens on
  the window-change event; the title change that follows the page load is inside the
  thirty-second interval, so the loaded page is never read. Keying the interval on the
  window title rather than only on the window would fix it, with a short floor to protect
  against the video player that retitles every second (see `VISIBLE_TEXT_INTERVAL`).

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
- `cargo test --workspace` — 302 passed, 0 failed, 11 ignored.
- `npm test` — 79 passed.
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

- **A UIAutomation walk is a cross-process call per step, and `FindAll` with
  `TreeScope_Descendants` is a whole-tree search inside the other process.** No element
  cap applies until after it returns, so it is not a bounded read at all. Using one to
  find the document element made `records_a_real_application_switch` time out at twelve
  seconds where it had passed in two: the collector was still reading one window when the
  next foreground change arrived, and a termination is only noticed at a foreground
  change. `uia::read_window` is now one breadth-first walk that finds the document and
  the text together, bounded by 80 elements, 4 levels, 24 children per node, and a 120 ms
  clock, reading a given window's text at most once every thirty seconds. Run the ignored
  desktop gate after touching anything the collector does per window — no unit test can
  see this. See AD-24.
- **A flag that classifies a failure is not a policy for handling one.**
  `InferenceError::is_transient` existed from the start, had its own tests, and no caller
  ever read it. One slow answer from Google ended a whole day's summarization run. If a
  predicate says something is worth retrying, find the code that retries on it before
  believing the behaviour is there. See AD-28.
- **A bounded walk can be bounded in the wrong dimension.** The visible-text read looked
  like a latency problem for a whole session — it returned one line from Chromium windows
  and the wall clock was the obvious suspect. It was not: 120 ms, 250 ms and 2 s all give
  the same one line. Measure which budget is actually running out before changing one.
  `FindAll(TreeScope_Children)` on a Chrome window returns two children; the page is ten
  or eleven levels below, behind unnamed panes.
- **A live gate that has always passed is not evidence that the code has no race.**
  `records_a_real_application_switch` passed for months over a termination race that a few
  tens of milliseconds of extra work in an unrelated read exposed immediately. When a
  desktop gate starts failing after a change that should not touch it, suspect a race the
  change merely revealed, and fix the race rather than restoring the old timing.
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
- Cloud providers are still mocked in every test. The one real call anybody has made was
  Google in the installed build, and it timed out at 60 seconds — which is what AD-28
  came from. Nothing in the suite talks to a real endpoint.
- No GGUF has ever been downloaded, and `llama-server` has only ever been a stub.
- Nothing checks that the five catalog repositories still exist; that is a release step.
