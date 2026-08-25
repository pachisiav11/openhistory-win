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


## AD-22: Idle time is measured; absence is not

**Decision.** A daily rollup carries `idle_ms` beside `active_ms`. Idle is the part of an
episode's span there was no evidence for: a window sat in front and nothing happened in
it. The two together are the day's screen time. Time between episodes is in neither
term, and no application is ever credited with idle time.

**Why.** AD-11 already separates an episode's elapsed time from the part there is
evidence for, and the rollup deliberately measured with the second. That is right for
"how long did I work on this" and wrong for "how long was I at this machine", and the
Day view only ever answered the first. A person who spent an hour reading a specification
saw ten minutes and concluded the recorder was broken.

The distinction that matters is between idleness and absence, and the episode boundaries
already draw it. A lock, a sleep, or a silence longer than `IDLE_SPLIT` closes an
episode, so every one of those stretches falls *between* two episodes. Anything left
*inside* an episode is time the machine was in use. Summing `duration_ms - active_ms`
therefore counts exactly the sitting-and-reading time and none of the walked-away time,
without the rollup needing to see the raw events at all.

Idle belongs to no application even though the episode it came from names one. Crediting
Word with the forty minutes a document sat open untouched would make the per-application
ranking a measure of what was left open rather than what was used, which is the failure
mode the active/elapsed split exists to prevent.

**Consequence.** Screen time understates a long, quiet stretch: a silence beyond fifteen
minutes ends the episode and is then counted nowhere. That is the conservative direction.
The numbers can say less happened than did; they cannot say more happened than did.

`idle_ms` is deliberately not `#[serde(default)]`. A stored report written before the
field existed fails to parse, and `Processor::load_day` already treats an unreadable
derived file as a reason to rebuild the day from the event log rather than as a failure.
Defaulting it would have been quieter and would have made every past day report no idle
time at all, which reads as a broken feature rather than an old file.

## AD-23: A search result opens the hour it happened in

**Decision.** Clicking a search result switches to the Day view for that episode's date
and scrolls to the hour the episode started in, marking that hour. The Day view treats
the hour as a request it consumes: it marks the hour, then tells the shell it has
arrived, so clicking the same result again asks again.

**Why.** A result already knew its date and its time, and opening the day threw the time
away — on a day with fourteen hours of rows, that is a page of scrolling to find the
thing that was just clicked. The hour is derived from the episode's start in local time,
because that is how the rollup files it.

Consuming the request is what makes a repeat click work. Held as ordinary state, the
second click sets the value it already holds, React sees no change, and nothing happens —
a control that works once and then appears broken. The mark itself is state in the Day
view, so it outlives the request and survives the day being reprocessed underneath it.

**Consequence.** The mark is cleared by changing the date and by nothing else. It stays
while the day refreshes and while a summary is written, which is what someone who came
from a search wants: the hour they asked about stays findable.

## AD-24: The visible text is bounded and redacted where it is written, not where it is read

**Decision.** The collector reads two more things from the accessibility tree: the
document a window is on, and a bounded sample of the text that window is displaying. Both
come from one walk, capped by elements visited (80), *named* depth (4), children queued
per element (24), lines kept (12 of 120 characters, 1,000 in total) and a 120 ms wall
clock,
with the text read at most once every thirty seconds per window. Every line passes
`oh-collector::text` before it reaches the log: password and offscreen elements are never
visited, runs of twelve or more digits are masked, and a long unbroken word that looks
like a key, a token or a PEM header is dropped whole.

**Why.** This is the capability the original macOS OpenHistory has and the port did not,
and it is the difference between a summary that says "Visual Studio Code for forty
minutes" and one that says what was being worked on. The frozen event schema already
reserved `document` and `visibleText`, so nothing about the contract had to move.

The caps are on the collector thread for a reason. Every step of a UIAutomation walk is a
cross-process call into the application being read; an unbounded walk of a large document
view can take longer than the interval between window switches, and the collector would
then be measuring its own latency. Bounding the walk by breadth *and* depth means a
pathological tree costs a known amount rather than a proportional one.

The clock is there because counting is not enough, and the desktop gate proved it. The
first version asked UIAutomation for every `Document` descendant in one `FindAll` call,
which is a whole-tree search executed inside the other process — none of the element caps
applied until after it returned. `records_a_real_application_switch` went from passing in
two seconds to timing out at twelve, because the collector was still reading one window
when the next foreground change arrived, and a termination is noticed at the next
foreground change. One bounded, breadth-first walk now finds the document and the text
together, and stops on whichever budget runs out first. A test that drives the real
desktop is what caught a regression no unit test could see.

The depth budget counts levels that named something, not levels of the tree. The first
version counted every level, and on Chromium and Electron it therefore reached nothing at
all: a Chrome window's page text sits ten or eleven levels below the window element,
behind a spine of seven unnamed `Pane` elements that exist only to hold the next one. The
installed build recorded `["Claude"]` and `["Notepad"]` — the window's own name and
nothing else — from windows full of text, and the browser gate now in this suite
reproduces it. The wall clock was the suspect and was measured first: raising it from
120 ms to 250 ms changed nothing, and neither did two seconds, because the walk was not
running out of time but stopping four levels into an empty corridor. Skipping unnamed
containers costs about ten of the eighty elements on Chromium and leaves the budget
meaning what it was written to mean — how far into *content* the read may reach — so a
spreadsheet is no more exposed than it was before.

A read that comes back with nothing but the window's own name no longer starts the
thirty-second clock. Chromium builds its accessibility tree when a client first reaches
into it and paints the page after the window exists, so the read taken the instant a tab
opens is the one that finds nothing; holding that answer for half a minute meant the
window was never read again while the user was on it.

Redaction happens at write time rather than at read time because the alternative is a log
that contains a secret and a promise that nobody will look. AD-14 already keeps a
separate type for what may leave the machine, but that boundary protects the network, not
the disk. Somebody with a text editor and the data folder is a case the redaction pass has
to hold on its own.

**Consequence.** The pass is deliberately eager: a genuine word of prose that happens to
be twenty characters of mixed letters and digits is dropped along with the tokens. That
trade is written into the tests, because the failure it prevents — an API key sitting in
a plain-text log forever — is not symmetrical with the failure it causes, which is one
missing line of context in one hour's summary.

Both categories are switches in Settings, defaulting on. Turning one off stops it being
read at all rather than filtering it afterwards, so there is no state in which the
collector holds something the settings say it should not have.

## AD-27: An application that leaves the foreground is asked about again

**Decision.** The collector keeps one slot for the application the foreground has just
left. `close_previous_application` asks about that slot as well as about the application
now in front, and the two-second liveness timer therefore re-asks about a departure whose
process was still exiting when it happened.

**Why.** Closing a window hands the foreground on before the process has finished exiting.
The liveness check made at that moment can still answer "running", and the pid was then
overwritten by the application that took the foreground — so nothing ever asked about it
again and no `applicationTerminated` was ever reported. The timer existed precisely to
notice an exit by asking rather than by waiting to be told, but the thing it was asking
about had already been forgotten.

This was a live race, not a theoretical one, and it surfaced as a two-in-three failure of
`records_a_real_application_switch` the moment the visible-text read in AD-24 got deeper
and shifted the timing by a few tens of milliseconds. The test had passed for months
against the same defect. A race that only one side of a timing change can see is worth
removing rather than tuning: one slot for the most recent departure is enough, because it
is the application the user just left.

**Consequence.** A termination is reported up to two seconds after it happened rather than
at the foreground change, which is far below the five-minute gap that ends an episode, so
nothing downstream can tell the difference.

## AD-28: A summary is retried when the reason it failed might not last

**Decision.** `InferenceService::generate` sends a prompt up to three times, pausing two
seconds and then four between attempts. Only failures `InferenceError::is_transient`
already recognised — a 429, a 5xx, a dropped connection, a timeout — are retried; a
missing key, a refused prompt or a 400 is reported on the first answer. Google is given
120 seconds to answer where Anthropic and OpenAI are given 60.

**Why.** The flag existed and nothing read it. `is_transient` was written to tell a blip
apart from a wall, was covered by its own tests, and no caller ever asked. A single slow
answer from Google therefore ended a whole day's run with "google did not answer within
60s" — and because `summarize_day` stops at the first failure, one blip in the ninth hour
threw away the rest of the day.

The timeout is per-provider because the providers are not asked for the same work.
`google.rs` adds 4,000 tokens of `maxOutputTokens` headroom that neither of the others is
given, because Gemini reasons before it answers and that reasoning is generated on the
same request and counted against the same ceiling. Sixty seconds is comfortable for a
300-token summary and not reliably enough for the thinking in front of it. Raising the
shared constant would have given Anthropic and OpenAI a longer deadline they have no use
for, and a deadline is only useful if it is short enough to mean something.

Retrying is bounded rather than persistent because a summary is not worth an unbounded
spend of someone's quota. Three attempts and six seconds of pauses is the difference
between absorbing a blip and hiding an outage.

**Consequence.** A provider that is genuinely down is now reported after three attempts
rather than one, so the worst case for a single hour is three timeouts plus the pauses.
That is bounded, it only happens when the run was going to fail anyway, and the run still
keeps every hour written before it.

The pauses are compiled out under `cfg(test)`. Tests assert what is retried and how many
times, never how long the pause was, and sleeping would cost every test that exercises a
failure.

## AD-29: Where a summary is written is a separate question from which model writes it

**Decision.** Settings asks first where summaries are written — nowhere, on this machine,
or a cloud provider — as three radio buttons, and only then which model. The place is the
question the user is answering; the vendor follows from it. AD-5's "one dropdown" applies
within the cloud, where the vendor really is a consequence of the model, and no longer
across the three places.

Alongside it, `inference.local_server_path` records where `llama-server` is, chosen
through a file dialog.

**Why.** The one-dropdown design put "No summaries", seven hosted models and any
downloaded ones in a single list. Reading it, the difference that matters most — whether
anything leaves the machine — was the difference between two adjacent `optgroup`s. A
person looking for local inference had to know to scroll past three cloud vendors to find
it, and the local models only appeared at all once something had been downloaded, so on a
fresh install the option was invisible.

The binary is the other half. Nothing ships `llama-server`, so `find_binary` looks beside
the executable and then on `PATH`, finds nothing on an ordinary machine, and readiness
said to put it on `PATH` — a sentence that asks the user to change their environment for
one application. A downloaded 3 GB model was unusable with no way to say where the server
lived. Now there is one.

**Consequence.** Choosing "On this machine" with nothing downloaded still moves the radio
and then explains what to download, rather than refusing the click. A choice that silently
does nothing is worse than a choice that admits it is not finished, and the first version
of this did exactly that.

Choosing a model on this machine selects the local provider, the way choosing a hosted
model selects the vendor that runs it. `use_local_model` used to set the identifier and
the path and leave `provider` alone, which the one-dropdown design hid: the list showed
the local model as chosen while every summary still went to whichever cloud was selected
before. The three-way radio made it visible — it read the provider, saw a cloud vendor,
and snapped back — and the installed config proved it, carrying `localModelId`,
`localModelPath` and `provider: anthropic` together. The pairing is now a function with a
test rather than a line inside a command, because the cloud side of it has always been
right and the two have to stay symmetric.

A path that has been moved or deleted reads as missing rather than being handed to the
spawner, so the failure is reported where it can be acted on instead of as a server that
would not start.

## AD-25: A saved day is a document, and the only thing the application will not delete

**Decision.** The Summary view composes a day — its written summary, where the time went,
and each hour that was written — into one Markdown document and keeps it under
`library/`, listed and read in the application, exported anywhere through a Save-as
dialog, and removed only by a confirmed click.

**Why.** AD-12 makes everything after the event log derived and disposable, and the
retention window eventually takes the events too. That is the right default for a
recorder and the wrong one for the day somebody actually wants to keep. The library is
the seam between the two: below it, everything is rebuildable and expendable; above it,
one file that no amount of reprocessing, forgetting or expiry will touch.

Markdown, not JSON, and composed rather than dumped. A saved day is for reading later,
possibly by a person who no longer has the application, so it carries the summary beside
the measurements that make it mean something and names the model that wrote it. Front
matter carries the title, the date and the time it was saved, which is enough for the
list without opening a file.

Composition lives in the Tauri layer rather than in `oh-core` because it needs both the
day's report and the day's summary, and the store deliberately knows about neither: it
stores a title, a date and a body. That keeps the store testable without a processor and
lets the document's shape change without touching what is already on disk.

**Consequence.** The application ships a second Markdown parser, `src/lib/markdown.ts`,
covering exactly the grammar `compose` emits. A full library would be a dependency
carried to read a file the application wrote itself, and the reason for storing Markdown
in the first place is that the file is legible without one.

## AD-26: The recording statement lives in the panel that does the recording

**Decision.** The list of what is recorded and what never is appears at the top of the
Recording panel in Settings, above the switches it describes, and again in the README. It
is written from the code, not from the intent.

**Why.** A privacy policy in a document nobody opens is a claim; the same words directly
above the switches that implement them are a contract the reader can check on the spot.
Placing it above the switches also orders the panel correctly: what the application does,
then what you may turn off.

Writing it last was deliberate. The statement was composed after the capture, the
redaction and the switches were finished, so each line names something that exists —
including the ones that had to be worded around a limitation, like idle time being
inferred from gaps rather than measured from input, because nothing in this application
watches the keyboard or the pointer at all.

**Consequence.** The statement and the code have to move together, and the tests hold two
of its lines so the panel cannot quietly lose it. The cloud-agreement text is now the
third place that has to agree, since document names and lines of on-screen text now reach
a provider when a cloud model is chosen.

## AD-30: The application fetches its own local runtime, and writes yesterday's summary itself

**Decision.** `llama-server` is no longer something the user has to find. Choosing "On
this machine" with no server already present fetches the pinned llama.cpp CPU build
(`oh-inference::runtime`) into the same data folder the model catalog uses, over the same
resumable download and the same `DOWNLOAD_EVENT` progress the models already report,
under a reserved identifier (`RUNTIME_ID`) so the settings page has one progress bar
rather than two. The archive is unpacked whole — every DLL, not just the 9 KB launcher —
because the work is in `llama-server-impl.dll`, `ggml-base.dll` and fifteen
instruction-set-specific `ggml-cpu-*.dll` files chosen at run time, and guessing which
subset a given machine needs fails as a missing DLL at spawn time. `Find…` remains, for
anyone who already has a build and would rather point at it than fetch another.

Alongside it, a background task (`auto_summary`) checks every thirty minutes and, once
the local clock reads past 05:00 and `inference.auto_summarize` is on, calls the same
`summarize_day` the day view calls, for yesterday, with whichever provider is already
chosen. Nothing new was built to make this safe to call on a timer: `summarize_day` was
already idempotent (AD-28's stale-hour check means a day already summarized and unchanged
costs nothing but a disk read), so the scheduler does not need to remember what it has
already done — asking again is free when there is nothing left to do.

The day prompt was also rewritten to ask for three or four times the previous length: the
detailed body grew from four-to-six sentences to three paragraphs of twelve-to-eighteen,
each instructed to name something concrete from the log rather than characterize the time
in general terms, and a further instruction bans restating the totals the prompt already
supplied. A fourth, explicitly separate paragraph — four to five sentences, after a blank
line, outside the detailed account — closes with what the day added up to as a whole.

**Why.** A downloaded model was unusable until the user found a matching llama.cpp
release by hand, worked out which of thirteen Windows builds their machine needed, and
pointed the application at the right file inside it — a task AD-29 already made visible
but did not remove. Fetching it automatically finishes what AD-29 started: nobody should
have to know llama.cpp exists to use a model that runs on their own machine.

The morning scheduler exists because a summary that only appears when asked is a summary
most days never get one. Gating on 05:00 local rather than firing at the stroke of
midnight is a guess that whoever is still awake right after midnight is still living the
day the calendar just turned past, and writing its summary out from under them would
describe less of the day than actually happened.

The longer prompt exists because four to six sentences condensing a full day's hourly
summaries left most of the day unsaid — a paragraph has room to name what a sentence
does not. The explicit ban on restating totals and the instruction to name something
concrete in every sentence exist because the model prompt is not just "write more"; that
alone produces padding, hedged generalities that could describe any day.

**Consequence.** The pinned build (`BUILD = "b10612"`) carries the same AD-5 obligation
the model catalog already has: moving to a newer one is a release step, not a config
edit, because the asset name is not derived from "latest" and a wrong guess fails at the
worst moment, in front of a user who cannot tell why. The `#[ignore]`d
`the_pinned_asset_is_still_published` test is the gate to run before changing `BUILD`.

The archive's on-disk size (`APPROXIMATE_BYTES`) is a measured constant rather than
something asked of the server before the first byte arrives, so the settings page can
say what is about to be fetched before the download has told it anything.

## AD-31: The hour in the heading and the clock on its entries are the same clock

**Decision.** `render_episode` converts the stored stamp to local time before printing
it, and an episode that began before the hour or ran past it says so on its own line
along with the fact that its active total is not all this hour's. The hourly instruction
states plainly that the entries are this hour's activity, including those that overlap
its edges.

**Why.** Episodes are stored in UTC (`stamp` in `oh-processing::episode`) and the hourly
heading is built from `HourlyRollup::hour`, which is local. The clock was sliced straight
out of the stamp string — `episode.start.get(11..16)` — so the two disagreed by the
machine's offset. On a machine at UTC+5:30 an hour headed "between 20:00 and 20:59"
listed its entries at 15:04, and the model did one of two things: reported the UTC times
as though they were the truth, or refused the hour outright — "the log appears to contain
data from an earlier time of day than what was requested." Both are correct readings of a
prompt that contradicts itself. The bug was invisible in the tests because every fixture
wrote a UTC literal and asserted on the heading, which is why the fixtures now build
their stamps through the local zone: a test written with a fixed literal passes in London
and fails in Delhi.

The overlap note is the other half. An episode is listed under every hour it touched, so
even with the zones agreed, one that started at 20:58 and ran on appears under the 21:00
hour with a start time that is not in it. Unexplained, that reads as a misfiled entry.

**Consequence.** `SYSTEM` also now says that window controls, menu and toolbar labels and
other interface furniture are not activity, and that an entry carrying nothing but an
application name should produce one short clause rather than a padded sentence. AD-30's
"every sentence must carry something from the log" pushed in the opposite direction: with
a thin hour and nothing else to name, the model reached for what the accessibility tree
had captured and wrote that the window "was visible on screen with minimize and restore
controls available." A rule that demands substance in every sentence has to be paired
with one that says what does not count as substance, or it manufactures filler out of
whatever is nearest.

The day prompt's target is a word count (about 300) rather than only a sentence range,
because a range of sentences bounds structure and not length; the first version of AD-30
produced ~600 words that restated the same file and figure across several sentences.

## AD-32: A failing subprocess is quoted, not guessed at

**Decision.** `llama-server` is spawned with `stderr` piped and drained into a
twenty-line ring buffer, and a server that exits during startup is reported with the
line it actually wrote. No CORS flag is passed at all.

**Why.** Both halves of this were one failure. The spawn passed `--cors-allow-origin *`,
which llama.cpp has since renamed to `--cors-origins`; build b10612 rejects the old
spelling and exits immediately with `error: invalid argument: --cors-allow-origin`. That
message went to a `Stdio::null()`, so all the application could offer was a fixed
sentence: "llama-server exited while loading the model. The file may not be a valid GGUF,
or the machine may not have enough memory for it." Both guesses were wrong, and the
first sends somebody to re-download three gigabytes that were never at fault — the model
loads in under nine seconds once the argument is gone.

The flag should not have been there in any case. The comment beside it said the
renderer's fetch would be refused without it, but every request to this server is made
from the Rust side; the browser's origin rules were never in play. The current default
is `*` regardless, so dropping it changes nothing but the failure.

**Consequence.** A piped stderr must be drained or the child blocks once the pipe fills,
so the reader task is not optional bookkeeping — it is what keeps the server running. The
buffer keeps the tail rather than the head because llama.cpp writes a screen of banners
before it says anything useful, and `explain_exit` prefers the last line mentioning
"error" over the last line outright for the same reason.

This is the third instance of one pattern in this codebase, after `is_transient` and
`auto_summarize`: something that knows the answer, and nothing that asks it. Here the
subprocess knew exactly why it had died and was being told to be quiet.

## AD-33: A local model is asked not to think

**Decision.** Every request to `llama-server` carries
`chat_template_kwargs: { enable_thinking: false }`, and an answer that comes back empty
having spent its budget in `reasoning_content` is reported as that rather than as
silence.

**Why.** The fetched Gemma model is a reasoning model. Handed an hourly prompt it wrote
three hundred tokens of chain-of-thought that restated the instructions back to itself
("Constraint 1: Be concrete... Constraint 3: Reply with prose only"), hit the ceiling
mid-sentence with `finish_reason: "length"`, and returned `content: ""`. The parser reads
`content` and nothing else, so the run surfaced as "local returned an empty summary" —
accurate, and no help at all. Asked not to think, the same model on the same budget
answers in a third of the tokens.

Summarizing a day is not a problem that rewards deliberation. The material is already
reduced to a list of what was on screen and for how long; there is nothing to work out,
only something to say. Thinking here buys nothing and costs the entire output budget on
a small model, which is exactly the class of model somebody choosing local inference is
most likely to be running.

**Consequence.** The instruction goes in the request body, never as a spawn argument.
`--reasoning off` would do the same job for a server this application starts, but an
argument an older build does not recognise is fatal at startup — which is precisely how
`--cors-allow-origin` broke local inference in AD-32. An unknown field in the body is
ignored; an unknown flag on the command line is a dead server. The body also covers an
adopted process, which was started by somebody else and cannot be given arguments at all.

`reasoning_content` is parsed but never used as the summary. It is a working note
addressed to itself, not an answer, and printing it as a day's summary would be worse
than the empty string it replaces. It exists in the struct only so an empty `content` can
be explained.

## AD-34: A paragraph break is part of the answer

**Decision.** `tidy` keeps the blank lines a model puts between paragraphs, joining
lines only inside one, and the views render one `<p>` per paragraph rather than one
element for the whole summary.

**Why.** The day prompt asks for three paragraphs — what was done, what it means,
and a conclusion — because a single block of two hundred words is not read, it is
skimmed. The models delivered them. `tidy` then joined every line with a space on its
way to storage, and the interface put whatever survived into one `<p>`, where HTML
collapses what is left. Two independent layers erased the same structure, so the prompt
could be rewritten indefinitely without the reader ever seeing the difference.

The flattening was right when it was written. A summary was one paragraph then, and a
model that produced two had invented a shape nobody asked for. What changed is that
the shape is now asked for; what did not change is the code that assumed it never
would be.

**Consequence.** Hard-wrapped output is still unwrapped, because a model that breaks
its lines at eighty columns is describing its own margin, not the document's. The
distinction `tidy` now draws is between a blank line, which means a paragraph, and a
single newline, which means nothing.

Markdown in the library needed no change: `compose` already wrote the summary text
through unaltered, and blank lines are how Markdown has always separated paragraphs.
Summaries written before this are stored flat and stay flat — the structure was
lost at generation time, not at display time, so they have to be written again to gain
it.

## AD-35: One copy of the application, and one temporary per process

**Decision.** `tauri-plugin-single-instance` is the first plugin registered: a second
launch focuses the window that already exists and exits. Every atomic write goes
through `paths::write_atomically`, whose temporary file is named for the process that
wrote it.

**Why.** Saving reported `could not replace
%APPDATA%\openhistory-win\index\search-index.json`. Nothing was wrong with the index.
Two copies of the application were running, and six places in the tree wrote a
temporary at a fixed `<name>.writing` beside the destination. Both wrote the same
temporary; the first rename moved it onto the destination; the second found its own
source gone and failed. The message named the destination, which was intact, and said
nothing about the other process, which was the whole story.

The two halves of the fix answer different questions. The guard answers whether a
second copy should exist at all: it should not. An event log, a search index and a
settings file with two writers is not a window-management problem, it is corruption
waiting for the right interleaving — and the collector had no interlock either, so
both copies were recording the same desktop into the same day.

The per-process temporary answers what should happen anyway. A guard covers copies
this build starts; it does not cover an older build still running, a copy started
before the guard existed, or a second process a future feature introduces. With a
name per process the writes no longer collide and the later one wins, which is what
an atomic write was always supposed to mean. A rename that fails now deletes its own
temporary, because a per-process name would otherwise be left behind for good.

**Consequence.** Six copies of the same five lines became one function in `oh-core`,
which is where the failure can be described once. The plugin has no commands, so
there is no capability to grant. Launching from the tray, a shortcut or the installer
now raises the running window instead of starting a rival, which is also what somebody
double-clicking the icon expected in the first place.

## AD-36: A window is glanced at, and a few named windows are read

**Decision.** The visible-text read has two budgets. Every application gets the glance
it always had — four named levels, eighty elements, 120 ms, twelve lines. The
applications named in `recording.deepReadApps` get a study: eight named levels, nine
hundred elements, 500 ms, twenty-eight lines, the contents of any editing surface, and
a second walk seeded from the window's own child windows. Whatever either walk
collects is labelled `Writing`, `Content` or `Furniture`, and the line budget is filled
in that order.

**Why.** The read was returning window furniture and nothing else. A Word window came
back as `["final crit - Word", "DropShadowTop", "MsoDockTop", "MsoDockBottom"]` and a
Claude window as `["Claude", "Minimize", "Restore", "Close", "Menu", …]`. Three
separate causes, each of which had to be removed before the next one showed:

Breadth-first order reaches the frame first, and the frame is enormous — a Word ribbon
publishes several hundred named controls. Ordering the budget by control type rather
than by tree position fixes that, and it has to be by control type: a chat window's
sidebar of past conversations stands between the frame and the conversation and is
content by any test that does not ask what the element actually is. Hence three tiers
rather than two, with `Text`, `Document` and `Edit` above everything else.

Depth and breadth were tuned for recognising a window, not reading one. A chat message
sits six named levels down behind a sidebar several hundred elements wide; the eighty
elements a glance allows were spent long before the walk arrived. This is why the wider
budget is a list of applications rather than a switch. It costs up to half a second on
the collector thread, which pumps the WinEvent hooks, and it writes down more of what
was on screen — neither of which should apply to everything a person runs.

An editor's name is not its text. Word calls its editing surface "Page 1 content" and
publishes the page through a text range; a plain text box publishes it as a value. Both
are read, and an element whose value is a location is refused outright — a Chromium
document answers with its address as a value and its entire window as a range, in
document order, which is the navigation and then the sidebar and then, eventually, the
conversation. Only `Document` and `Edit` are asked at all: a `Text` element's name
already is its text, and asking one for a range made a Claude read a hundred copies of
the same page.

An embedded browser is a window of its own. A Tauri or WebView2 application publishes
a node called "… - Web content" with nothing under it until something asks that child
window directly, at which point Chromium builds the tree. The second walk exists for
that, and runs only after the window's own tree is exhausted: seeding it up front cost
an Electron window its entire clock on trees it had already published.

**Consequence.** A Word window now yields the essay's opening paragraphs and its
footnotes, a Claude window the messages in the conversation, and a Markdown Renderer
window the document and the recent-file list. Summaries can say what a file called
`final crit` was about, which is the difference between naming a day's work and
describing it. The cost is real: more of what was on screen is written to the event
log for these applications, an episode carries thirty-six lines rather than twenty, and
fourteen rather than eight reach a model. It is a list a person adds a name to, and
`captureVisibleText` still turns the whole of it off.

A line that names a file location is now dropped from screen text entirely. An Electron
window publishes the `file://` address of its own bundle as its document's name, which
put an executable path into the timeline through the one field that had no guard
against it.
