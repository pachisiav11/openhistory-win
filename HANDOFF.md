# Handoff — OpenHistory for Windows

Last updated 2026-08-22 02:10, at the end of Phase 3. Read this before touching anything.

## Stop instruction

**Phase 3 is complete, committed, tagged, and pushed. Work stops here.** Do not begin
Phase 4, 5, or 6 unless the user asks for it.

## Where the project stands

| Phase | State | Tag |
| --- | --- | --- |
| 0 — Repo, workspace, CI, docs | Done, pushed | `phase-0` |
| 1 — Native activity collector | Done, pushed | `phase-1` |
| 2 — Tauri shell writing JSONL | Done, pushed | `phase-2` |
| 3 — Processing layer | Done, pushed | `phase-3` |
| 4 — Inference layer | Not started | — |
| 5 — MCP server | Not started | — |
| 6 — React frontend | Not started | — |

Repository: <https://github.com/pachisiav11/openhistory-win> (public, account `pachisiav11`).
Git flow agreed with the user: one commit per phase on `main`, tagged `phase-N`, pushed.

## What Phase 3 added

`crates/oh-processing` — four modules, all fed only by the event log:

- `episode.rs` — `detect_episodes(date, &events)`. Groups consecutive activity in one
  application. Splits on an application change, a privacy boundary, or `IDLE_SPLIT`
  (15 min) of silence. Ids are deterministic: `YYYY-MM-DD#<start_millis>`.
- `rollup.rs` — `roll_up(date, &episodes)` producing `DailyRollup` with a `HourlyRollup`
  per active hour. Hour attribution is proportional to overlap, and the rounding
  remainder goes to the first hour so the hours sum to exactly the daily total.
- `index.rs` — `SearchIndex`, an inverted index. AND semantics over terms, prefix
  matching. A private episode is indexed by application name only. URL query strings are
  never indexed.
- `day.rs` — `Processor`, `DayReport`. Writes `episodes/YYYY-MM-DD.json`, keeps
  `index/search-index.json`, and has `rebuild()` as the repair path.

Wiring: `Processor` lives in `AppState`; IPC commands `day_report`, `search_history`,
`rebuild_history`. The Today view now renders episodes with a summary line instead of raw
events, and waits 800 ms after the last status push before reprocessing.

Two decisions are written up in `docs/ARCHITECTURE.md`:

- **AD-11** — an episode carries `duration_ms` (elapsed) and `active_ms` (evidenced)
  separately. The plan's single 5-minute gap was wrong for a foreground collector; two
  thresholds fix it. Found by failing tests, not by reasoning.
- **AD-12** — everything after the event log is derived and disposable. Freshness is
  decided by comparing modification times, not by a timer.

## Test state at the stopping point

Everything below was run and passed:

- `cargo fmt --all` — clean.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test --workspace` — 88 passed, 0 failed.
- `cargo test -p oh-collector --test live_desktop -- --ignored --test-threads=1` — 4 passed.
- `cargo test -p openhistory-win --test persistence -- --ignored --test-threads=1` — 3 passed.
- `npm test` — 14 passed.
- `npm run build` — clean.
- UI checked in the browser preview: summary line, episodes newest first, durations, a
  private session named without any content, and the Pause/Resume toggle.

The Phase 3 gate itself is `a_recorded_session_becomes_searchable_episodes` in
`src-tauri/tests/persistence.rs`. It records two real applications in turn, then asserts
2+ episodes, `active_ms > 0`, hours summing to the daily total, searchability, and that
reprocessing is byte-identical.

**Known flake, not a defect:** `LNK1104: cannot open file …exe` sometimes appears when
relinking a test binary immediately after running it. Re-run the command; it links.

## If Phase 3 needs to be revisited

- `winver.exe` reports its application name as **"Version Reporter Applet"**, and its
  first window title is the raw format string `About %s`. The live gate derives its
  search term from the recorded data for this reason — do not hardcode application names
  in tests.
- Windows 11 Notepad is a **packaged** application. Spawning `notepad.exe` and killing
  the returned child leaves the real window open on the user's desktop. The gate uses
  `charmap.exe`, a plain Win32 executable, instead. Same trap applies to `calc.exe` and
  probably `mspaint.exe`.
- The search index holds what was **displayed** (application name, window title, URL
  path), never what was launched. Executable paths are on the episode but not indexed.

## Decisions already settled with the user — do not re-ask

- Architecture is my choice, optimised for quality, small download, and modest memory
  (explicitly "not like Electron"). Hence Rust + Tauri v2 + WebView2, no bundled JS runtime.
- Local inference: app-managed `llama-server`, spawned on demand, unloaded when idle, with
  in-app download and progress. Curated catalog plus a custom GGUF path.
- Model catalog, final: Gemma 4 E2B QAT (text-only), Gemma 4 E4B QAT (text-only), Qwen3.5-4B,
  Phi-4-mini 3.8B, Qwen3.5-2B. **No preselected default.**
- Anthropic provider: mock it in tests, flag the live call as an unverified gap. **Never ask
  the user to paste a secret into chat.**
- Privacy: nothing leaves the machine without an explicit opt-in.
- Scope was "all six phases, halt on a test-gate failure". That is now superseded by the
  stop instruction at the top of this file.

## Standing constraints

From the user's global `CLAUDE.md`:

- **Ask before deleting any file.** This is the only operation needing confirmation.
- **Never skip git hooks.** `--no-verify` is forbidden.
- Never force-push to `main`.
- Keep `todo.md` and `TODO_log.md` current.
- ASD-STE100 in chat only, never in file content.
- Verify UI changes in a browser before calling them done.

## Environment notes

- **No computer-use is available for this project.**
- **Browser-pane screenshots do not work here.** Use `get_page_text`, `read_page`, and
  `read_console_messages` instead. Appearance is therefore unverified — only structure
  and content are.
- Editing files while `npm run dev` is running can leave Vite serving a stale module and
  reporting `does not provide an export named 'default'`. Restart the dev server.

## Verification gaps carried forward

Recorded in `docs/ARCHITECTURE.md` AD-7, repeated here because they are easy to lose:

- Session lock and sleep are exercised with synthetic window messages, not a real lock.
- Private-browsing detection is verified live only on Chrome and Edge. Firefox, Brave,
  Opera, Vivaldi, and Arc are from documentation alone.
- Appearance is not verified — screenshots are unavailable in this environment.
- The tray icon is not driven by any test.
- The live Anthropic call is mocked, never exercised.
