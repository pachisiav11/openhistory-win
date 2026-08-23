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

## AD-5: The model catalog is curated, and every entry is verified against the live index

**Decision.** Settings lists five vetted models, each with the size of the exact file
that will be downloaded, measured against the host machine's RAM. No model is
preselected and nothing downloads until the user chooses.

**Catalog.**

| Id | Repository | File | Size |
|----|------------|------|------|
| `qwen3-1.7b` | `Qwen/Qwen3-1.7B-GGUF` | `Qwen3-1.7B-Q8_0.gguf` | 1.83 GB |
| `phi-3-mini` | `microsoft/Phi-3-mini-4k-instruct-gguf` | `Phi-3-mini-4k-instruct-q4.gguf` | 2.39 GB |
| `qwen3-4b` | `Qwen/Qwen3-4B-GGUF` | `Qwen3-4B-Q4_K_M.gguf` | 2.5 GB |
| `gemma-4-e2b-qat` | `google/gemma-4-E2B-it-qat-q4_0-gguf` | `gemma-4-E2B_q4_0-it.gguf` | 3.35 GB |
| `gemma-4-e4b-qat` | `google/gemma-4-E4B-it-qat-q4_0-gguf` | `gemma-4-E4B_q4_0-it.gguf` | 5.15 GB |

**Why.** An unfiltered model list is not a useful choice for someone who does not
already know what a quantization level is; a fixed single model is not enough for
someone who does.

**Why the entries must be checked against the index rather than written from memory.**
The first version of this catalog was written without network access, and three of its
five repositories did not exist: `Qwen/Qwen3.5-4B-Instruct-GGUF`,
`Qwen/Qwen3.5-2B-Instruct-GGUF` and `microsoft/Phi-4-mini-instruct-GGUF` are all
plausible names that no one publishes. The two that did exist were named differently
from what had been guessed — `google/gemma-4-E4B-it-qat-q4_0-gguf`, not
`google/gemma-4-e4b-it-qat-GGUF` — and their sizes were wrong by nearly a factor of
two, because the guessed figure was for a 4-bit quantization of the parameter count
rather than for the file the repository actually holds. Every row above was read off
the repository's own file listing.

`approximate_bytes` is therefore a recorded measurement, not a computed estimate. It is
what the progress bar divides by before the first response header arrives, so a wrong
value shows a download at 180% and then finishing.

**Consequence.** The catalog goes stale as vendors move and re-quantize their
repositories. `every_entry_is_complete_enough_to_download` checks the shape of each
entry offline; nothing in the test suite can check that a repository still exists,
because that would make the suite depend on the network. Re-checking the five listings
is a release step.

---

## AD-6: Crate layout

| Crate | Responsibility |
|-------|----------------|
| `oh-core` | ActivityEvent and Episode types, storage paths, JSONL reader and writer, configuration |
| `oh-collector` | Win32 hooks, UIAutomation reads, browser and privacy detection, session and power monitoring |
| `oh-processing` | Episode detection, hourly and daily rollups, inverted search index |
| `oh-inference` | Provider trait, the three cloud clients, llama-server lifecycle, prompt building, model catalog and downloader, credential storage |
| `oh-mcp` | HTTP server, bearer authentication, the sanitized history view |
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
- A real request to Anthropic, OpenAI or Google AI Studio. Every provider test runs
  against a local HTTP server that speaks each vendor's response shape, so the request
  body, the authentication header, the error mapping and the consent gate are covered,
  but no key has ever been used. What is unverified is whether each vendor still
  accepts that request shape.
- A real model download and a real `llama-server` run. The downloader and the process
  supervisor are tested against a local server and a stub executable. No GGUF has been
  fetched, and no summary has been written by a model rather than by a fake.
- That the five catalog repositories still exist. See AD-5.

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

## AD-11: An episode records elapsed time and evidenced time separately

**Decision.** Every episode carries two measurements. `duration_ms` is the wall-clock
span from its first event to its last. `active_ms` is the part of that span there is
evidence for: each silence between consecutive events contributes at most `ACTIVE_GAP`
(5 minutes). Rollups and the interface measure with `active_ms`. An episode ends only
after `IDLE_SPLIT` (15 minutes) of silence.

**Why.** The original plan used a single 5-minute gap: more than five minutes without
an event ended the episode. That is wrong for a foreground collector, which emits
events when the foreground *changes* and is otherwise silent. Ten quiet minutes reading
one file in one window produce no events at all, and a single threshold has to choose
between two bad answers — tear that stretch into two episodes, or credit an eight-hour
overnight gap as eight hours of work. Two thresholds answer both: the reading session
stays one episode, and the silence inside it stops being counted after five minutes.

This was found by the tests, not by reasoning. Two episode tests failed on the first
run of the Phase 3 suite, and the threshold was the reason.

**Consequence.** `active_ms <= duration_ms` always, and the two diverge whenever the
user works without switching windows. Anything reporting "time spent" must use
`active_ms`; `duration_ms` exists to place an episode on a timeline, not to measure it.
A day's hourly totals are apportioned from `active_ms` in proportion to each episode's
overlap with each local hour, with the rounding remainder given to the first hour, so
the hours always sum to exactly the daily total.

## AD-12: Everything after the event log is derived and disposable

**Decision.** Episodes (`episodes/YYYY-MM-DD.json`) and the search index
(`index/search-index.json`) are caches. The event log is the only thing that cannot be
regenerated. `Processor::rebuild` throws both away and derives them again from the log
alone, and a corrupt or unreadable day report is logged and treated as absent rather
than as an error.

A day's report is refreshed when the event log's modification time is later than the
report's. There is no processing timer and no incremental update.

**Why.** Today's report goes stale continuously while the collector is running, so
something has to decide when to redo it. A timer either wastes work on an idle machine
or lags behind a busy one; incremental update means two code paths that must agree
about episode boundaries, and the boundaries are exactly what is hard. Comparing two
modification times is one path, always correct, and costs two `stat` calls.

**Consequence.** Reprocessing a day is a full re-read of that day's log. That is
acceptable because a day is bounded — a heavy day is a few thousand lines — but it does
mean the interface must not ask for a report on every recorded event. The Today view
therefore waits 800 ms after the last status push before asking again. Episode ids are
derived from the day and the start instant (`YYYY-MM-DD#<start_millis>`) rather than
generated, so reprocessing produces byte-identical output and anything holding an id
keeps working.

---

## AD-13: One list of models, and the provider follows from the choice

**Decision.** Settings offers a single dropdown holding every model — three Anthropic,
three OpenAI, one Google, and whichever local models are installed — grouped by who
runs it. Choosing a model sets the provider. There is no separate provider control.

**Why.** A provider dropdown and a model dropdown have to agree, and every combination
where they do not is a state the user can reach and the code has to handle: an
Anthropic provider with a Gemini model selected, a cloud provider with no model, a
local provider with nothing downloaded. None of those mean anything. Deriving the
provider from the model makes them unrepresentable, and it matches how the choice is
actually made — a person picks Haiku or Sonnet, not "Anthropic, and then Haiku".

**Consequence.** The cloud entries are aliases that follow each vendor's newest release
rather than pinned dated model ids. That keeps the list short and keeps it current, at
the cost of the model changing under the user when a vendor ships. A summary records
the model that wrote it, so the history stays honest about what produced each line.

Each entry is labelled with whether its provider has a key, so "needs a key" is visible
before the choice rather than after it.

---

## AD-14: What leaves the machine is a separate type, not a filtered episode

**Decision.** `oh_processing::PublicEpisode` is the only shape anything outside the
process ever sees. It carries the application, the start and end, the active time, and
a title only when the episode is not private. Building one from an `Episode` is where
redaction happens: a private episode loses its title entirely, a URL loses its query
string and its fragment, and no file path is ever included. Both the inference layer
and the MCP server construct their payloads from `PublicEpisode` and have no access
path to the raw episode.

**Why.** Redaction written as a filter over the outgoing payload is a rule that has to
be remembered at every call site, and the cost of forgetting once is the entire privacy
guarantee in AD-4. Redaction written as a type conversion is a rule the compiler
enforces: there is no way to put a private title into a `PublicEpisode`, because the
field is not there to put it in.

This also means the two consumers cannot drift. The MCP server and the cloud prompt
builder redact identically because they redact through the same constructor, and
`nothing_private_reaches_the_provider` and the MCP server's own sanitization tests are
testing one implementation from two directions.

**Consequence.** Anything that legitimately needs the full episode — the timeline, the
search index, the day view — reads `Episode` directly and stays inside the process. The
boundary is the type, so adding a new outbound consumer means writing it against
`PublicEpisode` and getting the guarantee for free.

---

## AD-15: API keys live in the Windows Credential Manager, and never come back

**Decision.** Each provider's key is stored as one Credential Manager entry under a
per-provider target name. The window can store a key and can ask whether one exists. It
cannot read one. `api_keys` returns a status per provider — the provider name and
whether something is stored — and nothing else.

**Why.** The alternative is the configuration file, which is plain JSON in a readable
directory, next to a history the user is encouraged to open with ordinary tools. A key
in there is readable by every process running as that user and is trivially picked up
by a backup, a sync client, or a support request that asks for the config.

The keys never coming back to the window is a separate decision from where they are
kept. A "show key" affordance means the key crosses the IPC boundary into the WebView,
where it lands in a JavaScript string, a React state value, and any devtools session
that happens to be open. Nothing in the interface needs the value — the only operations
are "set" and "is one set" — so the value never has a reason to be there.

**Consequence.** A user who forgets which key they stored has to replace it rather than
check it. The field says the key is stored and that pasting a new one replaces it, so
the state is at least legible.

---

## AD-16: The MCP server stores a token's hash, never the token

**Decision.** Enabling the server issues a token, shows it once, and stores only its
SHA-256 digest. Authentication hashes the presented bearer token and compares digests
in constant time. There is no route, command or file that can return a token, and
regenerating one invalidates every earlier one.

**Why.** The token is a bearer credential for the user's entire activity history. A
stored token is a stored password, and this application already has a place where
long-lived secrets belong — the Credential Manager, per AD-15. A digest is not a
secret, so the token store can be an ordinary file next to the config without being an
asset worth stealing.

Constant-time comparison because the alternative leaks the prefix a byte at a time to
anything that can time a loopback request, which on this machine is everything.

**Consequence.** A user who loses the token cannot recover it and must regenerate,
which breaks whatever client was already configured. The settings panel therefore shows
the whole client configuration snippet at the moment the token is issued, so the normal
path is copy-once into the client rather than copy-later from the app.

---

## AD-17: The MCP server is loopback-only, off by default, and speaks both shapes

**Decision.** The server binds `127.0.0.1` on a preferred port, falling back to a free
one if that port is taken. It is off until the user enables it. It exposes plain REST
routes for a script and a JSON-RPC endpoint at `/mcp` for an MCP client, over the same
handlers. Whether it will answer about days other than today is a separate setting, and
every response is built from `PublicEpisode` per AD-14.

**Why.** Binding `0.0.0.0` on a personal machine puts a full activity history on the
local network behind one bearer token — a token that, per AD-16, the user has probably
pasted into a client configuration file. There is no use for this server that another
machine has to reach; a remote client can be given a tunnel deliberately.

Both shapes exist because the two consumers are genuinely different. An MCP client
wants `initialize`, `tools/list` and `tools/call`. A shell script or an editor extension
wants `GET /day/2026-08-22`. Sharing the handlers means they cannot disagree about what
the answer is.

**Consequence.** The port is not stable across restarts when the preferred one is
contended, so the client snippet is regenerated from the running server rather than
written once. The Tauri layer reconciles the server against the settings on every
change: it restarts when the port or the history setting moved, and does nothing when an
unrelated setting changed, so editing an exclusion list does not drop a client's
connection.

The window and the server share one `Processor` behind a mutex rather than each opening
their own. Two processors would each build their own in-memory search index from the
same files, and the one the window did not use would answer searches from whatever it
had loaded when it started.

---

## AD-18: The window runs outside Tauri

**Decision.** Every IPC call goes through one wrapper that uses Tauri's `invoke` when
the WebView is present and a registered mock otherwise. `src/lib/browser-mocks.ts`
answers every command with data shaped like the backend's, holding the same invariants:
a private episode has no title, a stored key never comes back, a token is shown once.
`npm run dev` opens a working application in an ordinary browser.

**Why.** The alternative is that no frontend change can be checked without building the
Rust side and launching a desktop session, which is both slow and impossible headlessly.
Tests run against the same wrapper with per-test mocks, so the frontend tests exercise
the real components rather than shallow renders.

The mocks holding the backend's invariants is the part that matters. A mock that happily
returned a title for a private episode would let a view be written that displays one,
and the test suite would pass. Making the mock refuse is what keeps the tests honest
about the guarantee in AD-4.

**Consequence.** There are two implementations of the IPC surface, and they can drift.
The mock is not generated from the Rust commands and nothing checks that the two agree —
a command renamed in Rust fails at runtime in the app while the browser build keeps
working. `npm run build` and `cargo build` are both in the gate, but the seam between
them is only covered by running the real application.

## AD-19: The collector never reads a window of its own process

**Decision.** `describe` refuses any window owned by this process, and the collector
signals that it has started before it looks at the window that is already in the
foreground rather than after.

**Why.** Reading a window is not a passive act. `GetWindowTextLengthW` and
`GetWindowTextW` send `WM_GETTEXTLENGTH` and `WM_GETTEXT` to the thread that owns the
window and wait for it to answer, and a thread only answers while it is pumping its
message queue. Between processes Windows short-circuits this — the documented reason
being that one hung program must not hang another — but within a process the message is
genuinely sent and genuinely waited on.

The collector runs on its own thread; the application's window belongs to the main one.
`Collector::start` blocks the caller until the hooks are up, and the caller at launch is
Tauri's `setup`, which runs on the main thread. So the ordering was: the main thread
creates and shows the window, calls `setup`, starts the collector, and waits. The
collector thread installs its hooks, asks which window is in front, gets ours, and asks
the main thread for its title. The main thread is waiting for the collector; the
collector is waiting for the main thread. Neither ever moves, the window never pumps
another message, and Windows draws it as "Not responding".

The WinEvent hooks were never exposed to this: they are installed with
`WINEVENT_SKIPOWNPROCESS`, so our own foreground changes and title changes never arrive.
The opening snapshot was the one path into the collector that the flag did not cover,
and it ran on every launch.

Releasing the caller before the snapshot is the second half. Describing a foreign window
can be slow — the privacy assessment walks another process's accessibility tree — and a
user interface must not wait on a stranger's window to finish starting up. Nothing about
the snapshot needs the caller to be blocked; only the hooks and the `collectorStarted`
event do, and both still happen first.

**Consequence.** OpenHistory's own window is absent from the history, which is right:
the timeline is a record of what the user was working in, and the window that displays
the timeline is not that. The two guards are independent — either one alone stops the
deadlock — and that is deliberate, because each also answers a failure the other does
not.

## AD-20: Windows starts the application, and it starts in the tray

**Decision.** One value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`,
named `OpenHistory`, holding the quoted path to this executable followed by
`--autostart`. On by default. The window is created hidden and shown by `setup`, except
when that flag is present.

**Why.** A history with a hole in it every time the machine restarts is not one you can
rely on, and the application is a tray program: the cost of it being there is a tray
icon. That is why the default is on rather than off.

Per-user, not per-machine. The history belongs to one account, the installer writes to
the user's own profile, and an entry under `HKLM` would need an administrator to set and
would start the application for people who never installed it.

The flag exists because a recorder that opens a 1280×800 window over whatever you signed
in to do is a nuisance. It is passed by the Run entry and by nothing else, so the same
executable opens normally when a person starts it and goes straight to the tray when
Windows does. Hiding the window after it appears would have been simpler and would have
shown a flash of it at every sign-in; creating it hidden and showing it in `setup` does
not. `setup` shows the window before it does anything else, because the work it does
afterwards takes long enough that a delay would read as a failure to start.

The registry is written at every launch, not only when the setting changes, so an entry
deleted by hand or left behind by an install at another path is corrected. The write is
skipped when the value is already right, so an ordinary launch touches the registry once
and then never again. `config.json` stays the setting the user changed; the registry is
the copy Windows reads.

**Consequence.** Windows' own startup manager can disable the entry independently — Task
Manager writes to `StartupApproved\Run` rather than deleting our value — and the
application cannot see that it has been overruled. Its settings will say it starts with
Windows while Windows quietly declines. Fighting that would mean re-enabling something
the user turned off in the operating system's own interface, which is worse.

## AD-21: Agreement to cloud summaries is a standing permission, not a step in a flow

**Decision.** The consent checkbox is always in the Summaries panel, whatever model is
chosen, and it says that nothing is sent while the model is "No summaries" or one on
this machine.

**Why.** It used to appear only once a cloud model was selected, which read as a sensible
progressive disclosure and was in fact a dead end: the model list is the first thing in
the panel, no model is preselected per AD-13, and a user who had not chosen one had no
way to agree to anything. The readiness line told them cloud summaries needed their
agreement while offering nowhere to give it.

Consent is not a step in choosing a model. It is a standing answer to "may this
application send a reduced description of my day to a company", and it is worth being
able to give or withdraw at any time, including while the answer has no effect. The
backend already treated it that way: `cloud_consent` is an independent field, checked at
the moment of sending rather than at the moment of choosing.

**Consequence.** The checkbox can be ticked while nothing would be sent, which is why the
label says so. Agreement given in that state is real and will apply the moment a cloud
model is chosen — the user has answered the question in advance, which is the point.

