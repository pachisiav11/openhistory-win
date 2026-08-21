# Architecture Decisions

This document records the decisions that shape the Windows port, and the reasoning
behind each one. `CONSOLIDATED.md` remains the reference for the event schema, the
storage layout, the MCP surface and the phase test gates; where this document
disagrees with it, this document wins and says why.

---

## AD-1: The backend is Rust, not C++ plus a Node sidecar

**Decision.** The collector, the processing layer, the inference orchestration and the
MCP server are all Rust, compiled into the Tauri binary. There is no C++ N-API addon,
no `node-gyp` build step, and no Node runtime in the shipped application.

**The plan this replaces.** `CONSOLIDATED.md` specified a C++ addon exposing seven
N-API functions, loaded by a Node process that Tauri would spawn as a sidecar, with
the processing, inference and MCP layers written in TypeScript on that same runtime.
That mirrors the macOS original, which is an Electron application and therefore
already had Node in the process tree.

**Why it changes.** Tauri's central advantage is that it has no bundled runtime — it
renders through the operating system's WebView2, which is already present on Windows
11. Adding a Node sidecar gives that advantage straight back:

| | Node sidecar design | Rust design |
|---|---|---|
| Runtime shipped | Node, roughly 50-80 MB | none |
| Processes at rest | two | one |
| Resident memory at idle | Tauri plus a Node heap of 40-60 MB | Tauri only |
| Build toolchain | Rust, MSVC, node-gyp, N-API headers | Rust, MSVC |
| IPC hops per event | addon to Node to Rust to WebView | collector to WebView |

An always-running background recorder is exactly the workload where a second idle heap
is least acceptable, and the stated goal for this port is to avoid Electron-class
overhead.

**What is not lost.** The C++ code would have called `SetWinEventHook`,
`IUIAutomation`, `RegisterPowerSettingNotification` and `WTSRegisterSessionNotification`.
The `windows` crate binds those same APIs directly, including the COM interfaces, so
the port is a language change rather than a capability change. Rust also removes the
manual `CoUninitialize` and `IUnknown::Release` discipline that the C++ version would
have needed on every early return.

**What is genuinely given up.** The seven-function N-API contract disappears as a
shipped interface. It existed so that JavaScript could drive the collector; nothing in
this port needs that. `crates/oh-collector` exposes the same seven operations as Rust
functions with the same names and semantics, so the contract survives as an API shape
even though the binding does not.

**Consequences.**

- The event schema is unchanged and must stay unchanged. See AD-2.
- TypeScript survives only in the frontend.
- Porting logic from the original TypeScript means rewriting it, not copying it. Every
  ported algorithm carries a unit test that pins its behaviour to the original's.

---

## AD-2: The ActivityEvent schema is frozen and shared with macOS

**Decision.** The JSON written to disk matches `CONSOLIDATED.md` field for field,
including `version: 1`, ISO 8601 timestamps and UUID v4 identifiers. Fields that
Windows cannot populate are omitted rather than emitted as null. `bundleId` is never
written on Windows.

**Why.** History files stay interchangeable between the two ports, and the schema acts
as a fixed contract between the collector and everything downstream. A serialization
test asserts the exact JSON shape so a careless struct edit cannot silently break it.

**Windows emits:** `collectorStarted`, `applicationActivated`, `windowChanged`,
`urlChanged`, `screenSlept`, `screenWoke`, `sessionLocked`, `sessionUnlocked`,
`privacyBoundary`, `applicationTerminated`.

**Deferred:** `focusedElementChanged`, `selectionChanged`, `textInput`,
`documentChanged`, `pointerClick`, `documentContextChanged`, `uiSnapshot`. These need
per-application UIAutomation event subscriptions with a poor cost-to-value ratio, and
several of them capture typed text, which raises a privacy question worth answering
deliberately rather than by default.

---

## AD-3: Local inference runs on demand and unloads when idle

**Decision.** The app manages a `llama-server` child process. It starts the server
when a summarization job is queued, waits for `/health` to report ready, generates,
and terminates the server after an idle timeout. The model is not held resident.

**Why.** A 3 GB model held in memory for a background utility is the exact overhead
this port exists to avoid. Summarization is a batch job that runs a few times an hour,
and its outputs are a few hundred tokens, so several seconds of model load time is
irrelevant to the user, who is never waiting on it.

**Consequence.** Interactive chat against the local model would feel slow under this
policy. If interactive use is added later it needs a keep-loaded mode, and that should
be an explicit setting rather than a silent change to this behaviour.

---

## AD-4: Nothing leaves the machine without explicit consent

**Decision.** Cloud summarization is off until the user turns it on and acknowledges
what gets sent. With no local model installed and no cloud consent, summaries do not
generate; the app records and browses history and says plainly that summaries are off.

**Why.** Window titles and URLs are among the most revealing data a machine holds. A
silent cloud fallback would ship that data off the device precisely when the user has
done nothing to authorize it.

**Consequence.** A first run with no configuration produces a working timeline with no
summaries. That is the intended experience, and the UI explains it rather than failing
quietly.

---

## AD-5: The model catalog is curated, with an escape hatch

**Decision.** Settings lists five vetted models with real sizes, measured against the
host machine's RAM, plus a file picker for any other GGUF. No model is preselected and
nothing downloads until the user chooses.

**Catalog:** Gemma 4 E2B QAT text-only, Gemma 4 E4B QAT text-only, Qwen3.5-4B,
Phi-4-mini 3.8B, Qwen3.5-2B.

**Why.** An unfiltered model list is not a useful choice for someone who does not
already know what a quantization level is; a fixed single model is not enough for
someone who does. Sizes are read from the Hugging Face API rather than hardcoded,
because published figures for these models disagree depending on quantization and on
whether the vision tower is included.

---

## AD-6: Crate layout

| Crate | Responsibility |
|-------|----------------|
| `oh-core` | ActivityEvent and Episode types, storage paths, JSONL reader and writer, configuration |
| `oh-collector` | Win32 hooks, UIAutomation reads, browser and privacy detection, session and power monitoring |
| `oh-processing` | Episode detection, hourly and daily rollups, inverted search index |
| `oh-inference` | Provider trait, Anthropic client, llama-server lifecycle, model catalog and downloader |
| `oh-mcp` | HTTP server, bearer authentication, response sanitization |
| `src-tauri` | Tauri application, IPC commands, tray, service wiring |

Dependencies point one direction: `oh-core` depends on nothing internal, every other
crate depends on `oh-core`, and `src-tauri` depends on all of them. Each crate is
testable without Tauri, which is what makes the phase gates runnable headlessly.

---

## AD-7: Verification runs without a human at the keyboard

**Decision.** Each phase gate is a program, not a checklist. Where a gate needs desktop
activity, the test creates it: it launches a real process, moves the foreground window
through the Win32 API, and asserts on the resulting event stream. Where a gate cannot
be automated, it is written down as an explicit gap rather than quietly assumed.

**Known gaps, requiring a person.**

- Session lock and unlock. Locking the workstation is a real side effect on the user's
  machine, so the handler is exercised by dispatching synthetic `WM_WTSSESSION_CHANGE`
  messages to the collector's message window instead.
- Screen sleep and wake, for the same reason, via synthetic `WM_POWERBROADCAST`.
- Per-browser URL extraction and private-window detection beyond whichever browsers
  are installed on the build machine. Chrome and Edge are verified live; Firefox,
  Brave, Opera, Vivaldi and Arc carry markers derived from documentation rather than
  from observation, and AD-8 explains why that distinction matters.
- Visual inspection of the rendered application window. The React views are driven
  headlessly against a mocked Tauri IPC layer and exercised in a real browser, which
  verifies behaviour and the accessibility tree but not appearance. Screenshots of the
  preview pane were not available in the environment this was built in.
- Clicking the tray icon and its menu. The tray is built and its handlers are ordinary
  functions, but no automated test drives a shell notification area.

---

## AD-8: Private browsing is detected from the accessibility tree, not the window title

**Decision.** A browser window is classified as private by reading its UIAutomation
accessible name — the window's own name plus the names of its immediate children — and
matching a per-browser marker against those. The Win32 title is still checked first as
a cheap fast path, but it is not the source of truth. When UIAutomation is unavailable
for a window, the browser is classified `Undetermined`, and an `Undetermined` browser
window is treated as private: the application is recorded, the title and URL are not.

**Why.** The original plan specified matching `" - Google Chrome (Incognito)"` against
the window title. That heuristic is obsolete. Current Chrome titles an incognito window
exactly as it titles an ordinary one — an incognito window on `about:blank` is titled
`about:blank - Google Chrome`, with no marker anywhere in the Win32 title. A
title-matching implementation therefore records private browsing in full while
appearing to work, which is the worst possible failure mode for the guarantee in AD-4.

The marker does still exist in the accessibility tree. Chrome's `BrowserRootView`
child element carries the accessible name `about:blank - Google Chrome (Incognito)`.
Edge publishes the marker in both places: its Win32 title reads
`about:blank - [InPrivate] - Microsoft Edge` and its root view reads
`about:blank - Microsoft Edge (InPrivate)`.

**Consequence.** Private-window detection now depends on a service that can fail.
Elevated windows and applications that refuse automation return nothing, so the
`Undetermined` state exists to make that failure fail closed. The cost is that a
browser window we cannot inspect is recorded with less detail than it could be; the
alternative is recording a private session in full, which is not a trade worth making.

**How this is held.** `private_browsing_records_nothing_but_the_boundary` in
`crates/oh-collector/tests/live_desktop.rs` launches every supported browser installed
on the machine in its private mode, against a throwaway profile, and asserts that the
stream contains a `privacyBoundary` and nothing else attributable to that process. It
is a live test because no unit test over title strings could have caught this — the
strings the unit test would assert on are the ones that turned out to be wrong.
Browser vendors change these trees, so `cargo run -p oh-collector --example uia_dump`
is kept in the repository to re-derive them.

---

## AD-9: History is append-only JSON Lines, cut on the local date

**Decision.** Events are appended to `events/YYYY-MM-DD.jsonl`, one JSON object per
line, flushed on every append. Nothing is ever rewritten in place. The day boundary is
the user's local date, not UTC. Retention defaults to keeping everything.

**Why.** Append-only is the failure mode that matters here: the process is expected to
be killed at shutdown rather than closed politely, and a crash mid-write can then cost
at most the event being written. A database would have to be opened, migrated,
compacted and repaired; a text log can be read with any tool the user already has, and
that transparency is worth more than query speed for a file this small. Flushing every
append costs nothing measurable at a few events a minute, and it means what is on disk
is what happened.

The local date matters because history is read as "what did I do yesterday". A UTC cut
would split an evening's work across two files for most of the world.

Retention defaults to unlimited because a personal history that silently deletes itself
is not one you can rely on. `prune` treats a retention of zero as "keep everything", so
a misconfigured setting cannot erase the archive.

**Consequence.** Reading a day is a linear scan. That is fine at this scale — a heavy
day is a few thousand lines — and Phase 3's episode files and search index exist so
that nothing downstream has to scan the raw log repeatedly.

**Threading.** The collector's callback runs on the same thread as its message loop and
WinEvent hook, so it must never block. The sink does nothing but hand the event to a
dedicated writer thread over a channel; the writer owns the store and is the only thing
that touches it. Stopping the service stops the collector first, then drops the sender
and joins the writer, so every observed event is on disk before `stop` returns.

---

## AD-10: The Windows shell is not an application

**Decision.** Windows whose class is one of a known set of shell surfaces — Alt-Tab
(`TaskSwitcherWnd`), Task View (`MultitaskingViewFrame`), the Start menu and search
(`Windows.UI.Core.CoreWindow`), the taskbar (`Shell_TrayWnd`), the desktop (`Progman`,
`WorkerW`) — are never reported as activity.

**Why.** These take the foreground constantly. Running the real application against a
live desktop produced a log where a third of the entries were `Task Switching` and
bare `Windows Explorer` activations from tapping Alt-Tab, which is noise the user did
not do and would have to read past on every timeline.

They cannot be excluded by process, because they are owned by `explorer.exe` — the same
process as a genuine File Explorer window, which is real activity. The window class is
the only thing that separates them.

**Consequence.** The list is empirical and will need additions as Windows changes its
shell. That is why it is a named constant with its own tests rather than an inline
match, and why `CabinetWClass` is in the tests as an explicit example of what must
still be recorded.
