# OpenHistory Windows Port — Consolidated Master Document

**Created:** 2026-08-21  
**Status:** Planning & architecture complete; implementation phases ready to execute.  
**Scope:** Port original OpenHistory (macOS Electron + Swift) to Windows 11 Tauri v2 + React + C++ + llama.cpp.

---

## Overview

Three documents describe this project:
- **todo.md** → Single task: Build Tauri v2 + React/TS + llama.cpp desktop app
- **tauri-llm-guide.md** → Self-contained guide for LLM integration layer (reusable blueprint)
- **build-plan.md** → Complete 6-phase execution plan for the full OpenHistory app

This consolidation links them with full architecture, implementation order, and test gates.

---

## Stack Architecture

| Component | Original (macOS) | Windows Port |
|-----------|-----------------|--------------|
| **Shell** | Electron | Tauri v2 |
| **Activity Collection** | Swift 6.1 + Accessibility APIs | C++ + Win32 + UIAutomation (node-addon-api) |
| **Native Bridge** | Swift + C (N-API, 7 exports) | C++ (N-API, same 7 exports) |
| **Storage** | JSONL files | JSONL files — identical schema |
| **Processing** | TypeScript | TypeScript — direct port |
| **Inference** | Apple on-device / OpenAI / Anthropic / Kimi | Anthropic API + llama.cpp (see tauri-llm-guide.md) |
| **MCP Server** | TypeScript | TypeScript — direct port |
| **Frontend** | React + TypeScript | React + TypeScript — direct port |

---

## System Architecture

```
Win32 WinEvent Hook (EVENT_SYSTEM_FOREGROUND)
  + UIAutomation (IUIAutomation, IUIAutomationElement)
  + Power/Session events (WM_POWERBROADCAST, WM_WTSSESSION_CHANGE)
        |
        v
C++ Native Addon (node-addon-api)
  - Normalizes all events to ActivityEvent JSON schema (version: 1)
  - Filters: private browsing, password fields, excluded apps
  - Thread-safe N-API queue (4096 event buffer) → JS callback
  - Exports 7 functions (same interface as original)
        |
        v (UTF-8 JSON strings via callback)
Tauri Main Process (Rust sidecar or embedded Node runtime)
  - Manages addon lifecycle
  - Writes events to JSONL: %APPDATA%\openhistory-win\events\YYYY-MM-DD.jsonl
        |
        v
TypeScript Service Layer
  - Episode detection (gap threshold: 5 min)
  - Hourly / daily rollup
  - Search index (in-process inverted index)
  - Inference orchestration
        |
        ├──► React Frontend (timeline, search, settings, day view)
        ├──► Inference Pipeline
        │    ├──► Anthropic API (production)
        │    └──► llama.cpp (local, see tauri-llm-guide.md)
        └──► MCP Server (localhost:47123, bearer token auth)
```

### LLM Integration Layer (tauri-llm-guide.md architecture)

The inference layer supports two providers:

**Anthropic (Production):**
- Direct API calls via `@anthropic-ai/sdk`
- Uses `claude-haiku-4-5-20251001` model
- Credentials stored via `tauri-plugin-stronghold` (encrypted)

**llama.cpp (Local):**
- Tauri Rust core spawns `llama-server` binary on app start
- React fetches directly via HTTP localhost (avoids Rust bottleneck)
- Streaming via Server-Sent Events (SSE)
- Architecture in tauri-llm-guide.md is self-contained and reusable

---

## Project File Structure

```
my-openhistory/
├── native/
│   └── collector/
│       ├── src/
│       │   ├── addon.cc              # N-API entry + 7 exports
│       │   ├── foreground_monitor.cc # SetWinEventHook
│       │   ├── foreground_monitor.h
│       │   ├── accessibility_reader.cc  # UIAutomation reads
│       │   ├── accessibility_reader.h
│       │   ├── browser_protection.cc    # private tab detection
│       │   ├── browser_protection.h
│       │   ├── session_monitor.cc       # power + session events
│       │   ├── session_monitor.h
│       │   └── activity_event.h         # struct + serializer
│       ├── binding.gyp
│       └── package.json
├── src/                          # React frontend
│   ├── main.tsx
│   ├── App.tsx
│   ├── views/
│   │   ├── Timeline.tsx
│   │   ├── Search.tsx
│   │   ├── DayView.tsx
│   │   └── Settings.tsx
│   └── hooks/
│       ├── useHistory.ts
│       └── useChat.ts (from tauri-llm-guide.md — for llama.cpp)
├── src/components/
│   └── ChatUI.tsx (from tauri-llm-guide.md — optional, for llama testing)
├── src-tauri/
│   ├── src/
│   │   ├── main.rs
│   │   ├── lib.rs
│   │   ├── collector_service.rs
│   │   ├── llm_server.rs (from tauri-llm-guide.md — if using llama.cpp)
│   ├── capabilities/
│   │   └── default.json         # Tauri v2 permissions
│   ├── Cargo.toml
│   └── tauri.conf.json
├── services/
│   ├── processing/
│   │   ├── episode-detector.ts
│   │   ├── hourly-rollup.ts
│   │   └── daily-rollup.ts
│   ├── inference/
│   │   ├── service.ts
│   │   ├── anthropic-provider.ts
│   │   └── llama-provider.ts
│   ├── search/
│   │   └── index.ts
│   └── mcp/
│       └── server.ts
├── tauri-llm-guide.md           # Reusable llama.cpp integration blueprint
├── build-plan.md                # Full 6-phase execution plan
├── CONSOLIDATED.md              # This file
├── todo.md
└── TODO_log.md
```

---

## ActivityEvent Schema (Canonical)

Must match exactly. Version always `1`. Date is ISO 8601.

```typescript
interface ActivityEvent {
  version: 1;
  id: string;           // UUID v4
  timestamp: string;    // ISO 8601
  kind: EventKind;
  application?: ApplicationDescriptor;
  windowTitle?: string;
  accessibilityTrusted?: boolean;
  pointerCaptureAvailable?: boolean;
  element?: SemanticElement;
  selectedElements?: SemanticElement[];
  textChange?: TextChange;
  browser?: BrowserObservation;
  document?: DocumentObservation;
  visibleText?: string[];
}

type EventKind =
  | 'collectorStarted'
  | 'applicationActivated'
  | 'windowChanged'
  | 'focusedElementChanged'
  | 'selectionChanged'
  | 'textInput'
  | 'documentChanged'
  | 'pointerClick'
  | 'urlChanged'
  | 'documentContextChanged'
  | 'uiSnapshot'
  | 'applicationTerminated'
  | 'screenSlept'
  | 'screenWoke'
  | 'sessionLocked'
  | 'sessionUnlocked'
  | 'privacyBoundary';

interface ApplicationDescriptor {
  name: string;        // display name
  path: string;        // full EXE path
  pid: number;
  bundleId?: string;   // omit on Windows
}

interface BrowserObservation {
  url?: string;
  isPrivate: boolean;
}
```

**Windows implements:** `collectorStarted`, `applicationActivated`, `windowChanged`, `urlChanged`, `screenSlept`, `screenWoke`, `sessionLocked`, `sessionUnlocked`, `privacyBoundary`. Rest are optional (stub as no-ops initially).

---

## N-API Bridge Contract (7 Exports)

The C++ addon must export exactly these functions (same names, same signatures as original):

```typescript
startCollector(callback: (eventJson: string) => void): void
stopCollector(): void
isTrusted(): boolean
requestTrust(): void                    // Windows: noop or show settings prompt
processIdentifier(): number             // current PID
canReadFocusedApplication(): boolean
bundleIdentifier(): string              // Windows: return process EXE name
```

Callback receives UTF-8 JSON strings one event at a time. Delivery is async via N-API thread-safe function (queue size: 4096). JS side never blocks.

---

## Storage Paths

```
%APPDATA%\openhistory-win\
  events\        YYYY-MM-DD.jsonl   (one file per day, append-only)
  episodes\      YYYY-MM-DD.json
  summaries\     YYYY-MM-DD.json
  index\         search-index.json
  config.json
  tokens.json    (MCP bearer tokens, hashed)
```

---

## Win32 APIs Reference (Phase 1: Collector)

| Event | API |
|-------|-----|
| Foreground app change | `SetWinEventHook(EVENT_SYSTEM_FOREGROUND, ...)` |
| Window title change | `SetWinEventHook(EVENT_OBJECT_NAMECHANGE, ...)` |
| Get current HWND | `GetForegroundWindow()` |
| Window title | `GetWindowTextW(hwnd, buf, len)` |
| Process ID | `GetWindowThreadProcessId(hwnd, &pid)` |
| EXE path | `OpenProcess` + `QueryFullProcessImageNameW` |
| Screen sleep/wake | `RegisterPowerSettingNotification` + `WM_POWERBROADCAST` |
| Session lock/unlock | `WTSRegisterSessionNotification` + `WM_WTSSESSION_CHANGE` |

### UIAutomation

```cpp
// Initialize COM and UIAutomation once at addon start
CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
IUIAutomation* pAutomation;
CoCreateInstance(CLSID_CUIAutomation, nullptr, CLSCTX_INPROC_SERVER,
                 IID_IUIAutomation, (void**)&pAutomation);

// For a given HWND:
IUIAutomationElement* pRoot;
pAutomation->ElementFromHandle(hwnd, &pRoot);

// Get window name:
BSTR name;
pRoot->get_CurrentName(&name);

// Find address bar in browser (Chrome/Edge):
// UIA_AutomationIdPropertyId == "toolbar" or specific id
// Then get text content → URL

// Detect password field:
// IUIAutomationElement::get_CurrentControlType == UIA_EditControlTypeId
// AND get_CurrentIsPassword == TRUE → set isSensitive = true
```

### Private Browsing Detection

| Browser | Method |
|---------|--------|
| Chrome | Incognito = `" - Google Chrome (Incognito)"` in title |
| Edge | InPrivate = `"InPrivate"` substring in title or accessible name |
| Firefox | Private = `"(Private Browsing)"` in title |
| Brave | `"Private Window"` in title |

If URL cannot be read: emit event with `browser.url = undefined`, `browser.isPrivate = false`.

### N-API Thread-Safe Delivery Pattern

```cpp
napi_threadsafe_function tsfn;  // global, initialized in startCollector

// From WinEvent callback thread:
void DeliverEvent(const std::string& json) {
    auto* data = new std::string(json);
    napi_call_threadsafe_function(tsfn, data, napi_tsfn_nonblocking);
}

// Called on JS thread:
void CallJS(napi_env env, napi_value jsCallback, void*, void* data) {
    auto* json = static_cast<std::string*>(data);
    napi_value arg;
    napi_create_string_utf8(env, json->c_str(), json->size(), &arg);
    napi_call_function(env, jsCallback, jsCallback, 1, &arg, nullptr);
    delete json;
}
```

Queue size: 4096. On overflow: drop oldest event, log to stderr.

---

## Tauri Configuration

### tauri.conf.json (Key Sections)

```json
{
  "productName": "OpenHistory",
  "version": "0.1.0",
  "identifier": "com.openhistory.win",
  "build": {
    "frontendDist": "../dist",
    "devUrl": "http://localhost:1420",
    "beforeDevCommand": "npm run dev",
    "beforeBuildCommand": "npm run build"
  },
  "app": {
    "windows": [{ "title": "OpenHistory", "width": 1280, "height": 800 }],
    "security": {
      "csp": "default-src 'self'; connect-src 'self' http://127.0.0.1:*"
    },
    "trayIcon": { "iconPath": "icons/icon.png" }
  },
  "bundle": {
    "active": true,
    "resources": {
      "native/collector/build/Release/collector.node": "resources/",
      "resources/llama-server.exe": "resources/"
    }
  }
}
```

**CSP note:** `connect-src http://127.0.0.1:*` is required for llama.cpp fetch.

### capabilities/default.json

Tauri v2 permissions:

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "Default capability",
  "windows": ["main"],
  "permissions": [
    "core:default",
    "shell:allow-spawn",
    "shell:allow-kill"
  ]
}
```

### Cargo.toml (src-tauri)

```toml
[package]
name = "my-openhistory"
version = "0.1.0"
edition = "2021"

[dependencies]
tauri = { version = "2", features = [] }
tauri-plugin-shell = "2"
tauri-plugin-stronghold = "2"  # for credential encryption
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }

[features]
custom-protocol = ["tauri/custom-protocol"]
```

### binding.gyp (native/collector)

```json
{
  "targets": [{
    "target_name": "collector",
    "sources": [
      "src/addon.cc",
      "src/foreground_monitor.cc",
      "src/accessibility_reader.cc",
      "src/browser_protection.cc",
      "src/session_monitor.cc"
    ],
    "include_dirs": ["<!@(node -p \"require('node-addon-api').include\")"],
    "libraries": ["-lole32", "-loleaut32", "-luiautomationcore", "-lwtsapi32"],
    "defines": ["NAPI_DISABLE_CPP_EXCEPTIONS"],
    "conditions": [["OS=='win'", {
      "msvs_settings": {
        "VCCLCompilerTool": { "AdditionalOptions": ["/std:c++17"] }
      }
    }]]
  }]
}
```

---

## Inference Layer (llama.cpp Integration)

**See tauri-llm-guide.md for complete, self-contained implementation.**

### Architecture (from tauri-llm-guide.md)

```
┌──────────────────────────────────────────┐
│  Tauri App Process                       │
│                                          │
│  ┌──────────────────┐ ┌──────────────┐   │
│  │  React Renderer  │ │  Rust Core   │   │
│  │  useChat hook    │◄►│              │   │
│  │  fetch SSE       │ │  Commands:   │   │
│  │  ChatUI          │ │   start_llm  │   │
│  └────────┬─────────┘ │   stop_llm   │   │
│           │           │   get_status │   │
└───────────┼───────────┴──────┬───────┘   │
            │                  │
            │ HTTP fetch       │ Child process
            v (localhost)      v spawn/kill
    ┌────────────────────────────────────┐
    │  llama-server (llama.cpp binary)   │
    │                                    │
    │  POST /v1/chat/completions (SSE)   │
    │  GET  /v1/models                   │
    │  GET  /health                      │
    └────────────────────────────────────┘
```

**Key design decisions:**
- React calls llama-server directly via fetch → avoids Rust bottleneck for streaming
- Rust manages llama-server child process lifetime (spawn on app start, kill on app close)
- Tauri IPC only used for process control, not LLM data

### Files from tauri-llm-guide.md (reuse in src-tauri/src/)

- **src-tauri/src/llm_server.rs** — Struct for managing llama-server subprocess
- **src-tauri/src/lib.rs** — Tauri command handlers: start_llm_server, stop_llm_server, get_server_status

### Files from tauri-llm-guide.md (reuse in src/)

- **src/hooks/useChat.ts** — React hook for streaming LLM completions
- **src/components/ChatUI.tsx** — Optional UI for testing llama.cpp directly

### Inference Service (TypeScript)

```typescript
import Anthropic from '@anthropic-ai/sdk';

export async function summarizeAnthropicAsync(
  prompt: string,
  apiKey: string
): Promise<string> {
  const client = new Anthropic({ apiKey });
  const msg = await client.messages.create({
    model: 'claude-haiku-4-5-20251001',
    max_tokens: 256,
    messages: [{ role: 'user', content: prompt }],
  });
  return (msg.content[0] as { text: string }).text;
}

export async function summarizeLlamaAsync(
  prompt: string,
  port: number
): Promise<string> {
  const response = await fetch(`http://127.0.0.1:${port}/v1/chat/completions`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      model: 'local',
      messages: [{ role: 'user', content: prompt }],
      max_tokens: 256,
      stream: false,
    }),
  });
  const data = await response.json();
  return data.choices[0].message.content;
}
```

### Prompt Templates

```
HOURLY:
"You are a work summarizer. Given these activity episodes from [HOUR]:
[EPISODES_JSON]
Write a 2-3 sentence summary of what was being worked on. Be concrete — name the apps, files, or topics."

DAILY:
"You are a work summarizer. Given these hourly summaries for [DATE]:
[HOURLY_SUMMARIES]
Write a concise daily summary (4-6 sentences) noting the key work done, major context switches, and any patterns."
```

### Credential Storage

API keys stored via Tauri's `tauri-plugin-stronghold` (encrypts at rest using Windows DPAPI). Never written to config.json in plaintext.

---

## MCP Server (Phase 5)

**Endpoint:** `http://127.0.0.1:47123`  
**Auth:** Bearer token (hashed comparison)

### Endpoints

```
GET /mcp/v1/health           → { ok: true }
GET /mcp/v1/today            → { episodes: [...], hourlySummaries: [...] }
GET /mcp/v1/summary/:date    → { date, dailySummary, hourlyBreakdown }
GET /mcp/v1/search?q=QUERY   → { results: Episode[] }
GET /mcp/v1/recent?n=10      → { episodes: Episode[] }
```

### Auth Implementation

```typescript
function authenticate(req: IncomingMessage): boolean {
  const auth = req.headers['authorization'] ?? '';
  const token = auth.replace('Bearer ', '');
  return validTokens.has(sha256(token)); // hashed comparison
}
```

### Sanitization Rules

- Drop any event where `isSensitive === true`
- Drop any event where `browser.isPrivate === true`
- Episode titles: include app name + window title, strip visibleText
- Never expose raw event arrays in MCP responses — summaries only

### Claude Code Integration

Add to `~/.claude/mcp.json`:
```json
{
  "mcpServers": {
    "openhistory": {
      "command": "curl",
      "args": ["-s", "-H", "Authorization: Bearer TOKEN", "http://127.0.0.1:47123/mcp/v1/today"]
    }
  }
}
```

---

## Processing Logic (Phase 3)

### Episode Detection

```typescript
const GAP_THRESHOLD_MS = 5 * 60 * 1000; // 5 minutes

function detectEpisodes(events: ActivityEvent[]): Episode[] {
  const episodes: Episode[] = [];
  let current: Episode | null = null;

  for (const event of events) {
    const ts = new Date(event.timestamp).getTime();

    if (!current) {
      current = openEpisode(event);
      continue;
    }

    const gap = ts - current.lastEventTs;
    const appChanged = event.application?.name !== current.app;

    if (gap > GAP_THRESHOLD_MS || appChanged) {
      episodes.push(closeEpisode(current, event));
      current = openEpisode(event);
    } else {
      current.events.push(event);
      current.lastEventTs = ts;
    }
  }

  if (current) episodes.push(closeEpisode(current, null));
  return episodes;
}
```

---

## Execution Plan (6 Phases)

### Phase 1 — Native Activity Collector

**Goal:** C++ N-API addon fires ActivityEvent JSON to JS callback on foreground changes, URL changes, screen sleep/wake, session lock/unlock.

**Test gate:**
```js
const addon = require('./build/Release/collector');
addon.startCollector((eventJson) => {
  console.log(JSON.parse(eventJson));
});
// Switch apps for 30 seconds. Expect applicationActivated events.
// Open Chrome incognito. Expect privacyBoundary event.
// Lock screen. Expect sessionLocked event.
setTimeout(() => { addon.stopCollector(); process.exit(0); }, 30000);
```

**Pass criteria:** events printed on each app switch, `isPrivate: true` on incognito, `sessionLocked` on lock.

---

### Phase 2 — Tauri v2 Shell

**Goal:** Tauri app boots, starts the collector addon, writes events to APPDATA.

**IPC Commands:**

```rust
#[tauri::command]
fn start_collector(app: tauri::AppHandle) -> Result<(), String>

#[tauri::command]  
fn stop_collector(state: State<AppState>) -> ()

#[tauri::command]
fn get_status(state: State<AppState>) -> serde_json::Value
// returns { running: bool, eventsToday: number, lastEventAt: string | null }
```

**collector_service.rs:**

Rust spawns a Node.js sidecar process that loads the addon:

```rust
use tauri::Manager;

pub fn start_collector(app: &tauri::AppHandle) -> Result<(), String> {
    let node = std::process::Command::new("node")
        .arg(app.path().resource_dir()?.join("collector-runner.js"))
        .arg(app.path().app_data_dir()?.to_str().unwrap())
        .spawn()
        .map_err(|e| e.to_string())?;
    // store handle for later kill
    Ok(())
}
```

`collector-runner.js` (bundled as resource): loads `collector.node`, calls `startCollector`, writes events to JSONL path.

**Test gate (Phase 2):**

- `cargo tauri dev` builds and opens window.
- After 2 min of normal use, `%APPDATA%\openhistory-win\events\<today>.jsonl` exists and has content.
- `stop_collector` terminates the sidecar cleanly.

---

### Phase 3 — Processing Layer

**Goal:** Episode detection, hourly/daily rollup, search index.

Port these files from original `src/main/` verbatim, then adapt:
- Replace Electron IPC with direct function calls
- Replace macOS paths with `%APPDATA%\openhistory-win\`
- Remove any Apple-specific imports

**Test gate (Phase 3):**

After 10 minutes of real use: run `processDay(today)` in a test script. Expect 2+ episodes in the output JSON. Hourly rollup shows time-in-app > 0.

---

### Phase 4 — Inference Layer

**Goal:** AI summaries (hourly + daily) via Anthropic or llama.cpp.

**See tauri-llm-guide.md for complete llama.cpp setup.** Anthropic is straightforward (SDK + API key).

**Test gate (Phase 4):**

Call `summarize()` manually with a hardcoded prompt and real API key. Expect a non-empty string back within 5s (Anthropic) or 30s (llama.cpp).

---

### Phase 5 — MCP Server

**Goal:** Local authenticated HTTP server at `http://127.0.0.1:47123`.

Port `src/main/agent-mcp-service.ts` from original. Key adaptations:
- Use Node.js `http` module (no Express dependency unless original uses it)
- Replace macOS paths
- Remove Apple-specific fields from output

**Test gate (Phase 5):**

```bash
curl -H "Authorization: Bearer YOUR_TOKEN" http://127.0.0.1:47123/mcp/v1/today
# Expect: JSON with episodes array
curl http://127.0.0.1:47123/mcp/v1/today
# Expect: 401 Unauthorized
```

---

### Phase 6 — React Frontend

**Goal:** Full UI. Port from original renderer, adapt IPC calls.

**IPC Adaptation:**

```typescript
// Original (Electron):
const result = await window.electron.ipcRenderer.invoke('get-status');

// Port (Tauri v2):
import { invoke } from '@tauri-apps/api/core';
const result = await invoke('get_status');
```

**Views:**

- **Timeline** — today's episodes grouped by hour. Each episode: app icon, time range, window title, expand for sub-events.
- **Search** — text input, results update on keystroke (debounced 300ms). Show episode card: app + title + timestamp.
- **Day View** — daily summary text (AI generated), hourly bars, top-apps list.
- **Settings:**
  - Inference provider: dropdown (Anthropic / llama.cpp / Disabled)
  - Anthropic API key: password input → stored via stronghold
  - llama.cpp model: file path picker
  - Excluded apps: multi-select list
  - Data: "Delete all history" button (confirm dialog)
  - MCP: show token, regenerate button, copy integration snippet

**Test gate (Phase 6):**

- Timeline shows today's episodes after 10 min of use.
- Search for "Code" returns VS Code episodes.
- Settings: enter API key, trigger hourly summary manually, summary appears in Day View.
- Excluded app added → no events from that app appear after restart.

---

## Build Prerequisites

```
Node.js 22
Rust (stable) via rustup
Tauri CLI v2: cargo install tauri-cli --version "^2"
node-gyp: npm install -g node-gyp
MSVC Build Tools (Visual Studio 2022 Build Tools, C++ workload)
Windows SDK 10.0.22000+
```

For llama.cpp integration (Phase 4):
- llama-server binary — build from source or use prebuilt release
- Place llama-server.exe in PATH for dev, bundle into resources/ for prod
- A GGUF model file (Llama 3, Mistral, Phi-3, etc.)

---

## Known Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| UIAutomation requires accessibility permission on some Windows configs | First-run flow: check `isTrusted()`, show instructions if false |
| Anti-cheat / DRM blocks UIAutomation reads | Catch COM errors, emit event with partial data, continue |
| Browser URL capture varies by version | Test against Chrome, Edge, Firefox; fall back gracefully |
| node-gyp requires MSVC build tools | Document prereqs; include prebuilt `.node` in repo for CI |
| llama-server CORS | Always pass `--cors-allow-origin *` when spawning |
| Port 47123 conflict | On startup, check if port is free; use next available if not |
| Port 8080 conflict (llama.cpp) | Use `find_free_port()` utility; store port for frontend |
| Server warmup time | Poll `/health` before enabling UI; never just `sleep` |
| Process leak on crash | Check for existing server on port at startup; reuse if healthy |
| Binary path issues | Always use `PathBuf`, never string concatenation |
| CSP blocking fetch | Ensure `connect-src http://127.0.0.1:*` in tauri.conf.json |

---

## Dev Workflow

```bash
# Terminal 1: Vite dev server
npm run dev

# Terminal 2: Tauri dev (spawns Rust + Electron-like window)
cargo tauri dev
```

In dev mode, `get_binary_path()` returns `"llama-server.exe"` — ensure it's on PATH or hardcode a local path temporarily.

---

## Production Build

```bash
cargo tauri build
# Output: src-tauri/target/release/bundle/nsis/openhistory_0.1.0_x64-setup.exe
```

Place `llama-server.exe` in `resources/` before building so it gets bundled.

---

## Execution Order

```
Phase 1 → test ✓
  ↓
Phase 2 → test ✓
  ↓
Phase 3 → test ✓
  ↓
Phase 4 → test ✓
  ↓
Phase 5 → test ✓
  ↓
Phase 6 → test ✓
  ↓
End-to-end: 10 min real use → timeline populated → MCP curl returns data
```

**Do not proceed to the next phase if the current phase's test gate fails.**

---

## References

- **tauri-llm-guide.md** — Reusable blueprint for llama.cpp integration (self-contained, copy directly into Phase 4)
- **build-plan.md** — Original detailed spec document (subsumed into this consolidation)
- **todo.md** — High-level task tracking (this consolidation becomes the expanded task spec)

---

## Document Metadata

- **Consolidated:** 2026-08-21
- **Status:** Architecture & planning complete; ready for Phase 1 implementation
- **Format:** Single master document linking todo, tauri-llm-guide, and build-plan without information loss
- **Target Audience:** Implementation team (agents or humans following the 6-phase plan)
