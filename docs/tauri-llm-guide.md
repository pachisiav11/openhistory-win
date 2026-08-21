# Tauri v2 + llama.cpp Integration Guide

> **For agent use.** This is a self-contained build blueprint. Pick it up in a new session and implement without additional context.
>
> Stack: Tauri v2 · React · TypeScript · llama-server (llama.cpp) · Windows 11

---

## Architecture

```
┌─────────────────────────────────────────────────────┐
│  Tauri App Process                                  │
│                                                     │
│  ┌───────────────────┐    ┌───────────────────────┐ │
│  │  React Renderer   │    │  Rust Core            │ │
│  │                   │◄──►│                       │ │
│  │  useChat hook     │    │  Commands:            │ │
│  │  fetch SSE        │    │    start_llm_server   │ │
│  │  ChatUI component │    │    stop_llm_server    │ │
│  └────────┬──────────┘    │    get_server_status  │ │
│           │               └──────────┬────────────┘ │
└───────────┼──────────────────────────┼──────────────┘
            │                          │
            │  HTTP fetch (localhost)   │  Child process spawn/kill
            ▼                          ▼
    ┌────────────────────────────────────────┐
    │  llama-server (llama.cpp binary)       │
    │                                        │
    │  POST /v1/chat/completions  (SSE)      │
    │  GET  /v1/models                       │
    │  GET  /health                          │
    └────────────────────────────────────────┘
```

**Design decisions:**
- React calls llama-server directly via `fetch` — not proxied through Rust. Avoids Rust becoming a bottleneck for streaming.
- Rust manages the llama-server child process lifetime (spawn on app start, kill on app close).
- Tauri IPC (`invoke`) only used for process control, not LLM data.

---

## Prerequisites

```bash
# Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Tauri CLI v2
cargo install tauri-cli --version "^2"

# Node 22
# (use nvm or direct installer)

# llama-server binary — build from source or use a prebuilt release
# Place llama-server.exe in PATH for dev, bundle into resources/ for prod
```

You need a GGUF model file. Any Llama 3 / Mistral / Phi-3 GGUF works.

---

## Project Scaffold

```bash
npm create tauri-app@latest my-tauri-llm -- --template react-ts
cd my-tauri-llm
npm install
cargo tauri dev  # verify it boots before touching anything
```

---

## Final File Structure

```
my-tauri-llm/
├── src/
│   ├── App.tsx
│   ├── hooks/
│   │   └── useChat.ts
│   └── components/
│       └── ChatUI.tsx
├── src-tauri/
│   ├── src/
│   │   ├── main.rs
│   │   ├── lib.rs
│   │   └── llm_server.rs
│   ├── capabilities/
│   │   └── default.json          ← Tauri v2 permissions
│   ├── Cargo.toml
│   └── tauri.conf.json
├── package.json
└── vite.config.ts
```

---

## src-tauri/tauri.conf.json

```json
{
  "productName": "my-tauri-llm",
  "version": "0.1.0",
  "identifier": "com.example.my-tauri-llm",
  "build": {
    "frontendDist": "../dist",
    "devUrl": "http://localhost:1420",
    "beforeDevCommand": "npm run dev",
    "beforeBuildCommand": "npm run build"
  },
  "app": {
    "windows": [
      {
        "title": "LLM Chat",
        "width": 1200,
        "height": 800,
        "resizable": true
      }
    ],
    "security": {
      "csp": "default-src 'self'; connect-src 'self' http://127.0.0.1:*; script-src 'self' 'unsafe-inline'"
    }
  },
  "bundle": {
    "active": true,
    "targets": "all",
    "resources": {
      "resources/llama-server.exe": "resources/"
    }
  }
}
```

> **CSP note:** `connect-src http://127.0.0.1:*` is required. Without it, fetch to llama-server is blocked silently.

---

## src-tauri/capabilities/default.json

Tauri v2 uses a capabilities/permissions system. Create this file:

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

---

## src-tauri/Cargo.toml

```toml
[package]
name = "my-tauri-llm"
version = "0.1.0"
edition = "2021"

[dependencies]
tauri = { version = "2", features = [] }
tauri-plugin-shell = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }

[features]
custom-protocol = ["tauri/custom-protocol"]
```

---

## src-tauri/src/llm_server.rs

```rust
use std::process::{Child, Command};
use std::path::PathBuf;

pub struct LlmServer {
    child: Option<Child>,
    pub port: u16,
}

impl LlmServer {
    pub fn new() -> Self {
        Self { child: None, port: 8080 }
    }

    pub fn start(&mut self, model_path: &str, binary_path: &PathBuf) -> Result<u16, String> {
        if self.child.is_some() {
            return Ok(self.port);
        }

        let child = Command::new(binary_path)
            .args([
                "--model", model_path,
                "--port", &self.port.to_string(),
                "--host", "127.0.0.1",
                "--ctx-size", "4096",
                "--n-predict", "-1",
                "--cors-allow-origin", "*",   // required for fetch from renderer
            ])
            .spawn()
            .map_err(|e| format!("Failed to spawn llama-server: {e}"))?;

        self.child = Some(child);
        Ok(self.port)
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    pub fn is_running(&mut self) -> bool {
        if let Some(ref mut child) = self.child {
            matches!(child.try_wait(), Ok(None))
        } else {
            false
        }
    }
}

impl Drop for LlmServer {
    fn drop(&mut self) {
        self.stop();
    }
}
```

---

## src-tauri/src/lib.rs

```rust
use std::sync::Mutex;
use tauri::{Manager, State};

mod llm_server;
use llm_server::LlmServer;

pub struct AppState {
    pub server: Mutex<LlmServer>,
}

fn get_binary_path(app: &tauri::AppHandle) -> std::path::PathBuf {
    #[cfg(debug_assertions)]
    {
        // Dev: expect llama-server.exe in PATH or current dir
        std::path::PathBuf::from("llama-server.exe")
    }
    #[cfg(not(debug_assertions))]
    {
        // Prod: bundled resource
        app.path()
            .resource_dir()
            .unwrap()
            .join("resources")
            .join("llama-server.exe")
    }
}

#[tauri::command]
fn start_llm_server(
    state: State<AppState>,
    app: tauri::AppHandle,
    model_path: String,
) -> Result<u16, String> {
    let binary = get_binary_path(&app);
    let mut server = state.server.lock().unwrap();
    server.start(&model_path, &binary)
}

#[tauri::command]
fn stop_llm_server(state: State<AppState>) {
    let mut server = state.server.lock().unwrap();
    server.stop();
}

#[tauri::command]
fn get_server_status(state: State<AppState>) -> serde_json::Value {
    let mut server = state.server.lock().unwrap();
    serde_json::json!({
        "running": server.is_running(),
        "port": server.port,
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState {
            server: Mutex::new(LlmServer::new()),
        })
        .invoke_handler(tauri::generate_handler![
            start_llm_server,
            stop_llm_server,
            get_server_status,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                // Kill llama-server on app close
                if let Some(state) = window.try_state::<AppState>() {
                    state.server.lock().unwrap().stop();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error running app");
}
```

---

## src-tauri/src/main.rs

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    my_tauri_llm_lib::run();
}
```

---

## src/hooks/useChat.ts

```typescript
import { useState, useCallback, useRef } from 'react';

export interface Message {
  role: 'user' | 'assistant' | 'system';
  content: string;
}

interface UseChatOptions {
  port?: number;
  systemPrompt?: string;
}

export function useChat({ port = 8080, systemPrompt }: UseChatOptions = {}) {
  const [messages, setMessages] = useState<Message[]>([]);
  const [streaming, setStreaming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const abortRef = useRef<AbortController | null>(null);

  const send = useCallback(async (content: string) => {
    if (streaming) return;

    const userMsg: Message = { role: 'user', content };
    const history: Message[] = [...messages, userMsg];

    const payload = {
      model: 'local',
      messages: systemPrompt
        ? [{ role: 'system', content: systemPrompt }, ...history]
        : history,
      stream: true,
    };

    setMessages([...history, { role: 'assistant', content: '' }]);
    setStreaming(true);
    setError(null);

    abortRef.current = new AbortController();

    try {
      const response = await fetch(`http://127.0.0.1:${port}/v1/chat/completions`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
        signal: abortRef.current.signal,
      });

      if (!response.ok) {
        throw new Error(`llama-server error: ${response.status}`);
      }

      const reader = response.body!.getReader();
      const decoder = new TextDecoder();
      let assistantText = '';

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        const chunk = decoder.decode(value, { stream: true });
        for (const line of chunk.split('\n')) {
          if (!line.startsWith('data: ')) continue;
          const data = line.slice(6).trim();
          if (data === '[DONE]') continue;
          try {
            const parsed = JSON.parse(data);
            const delta = parsed.choices?.[0]?.delta?.content ?? '';
            assistantText += delta;
            setMessages(prev => [
              ...prev.slice(0, -1),
              { role: 'assistant', content: assistantText },
            ]);
          } catch {
            // malformed SSE chunk — skip
          }
        }
      }
    } catch (err: any) {
      if (err.name !== 'AbortError') {
        setError(err.message ?? 'Unknown error');
        setMessages(prev => prev.slice(0, -1)); // remove empty assistant msg
      }
    } finally {
      setStreaming(false);
    }
  }, [messages, streaming, port, systemPrompt]);

  const stop = useCallback(() => {
    abortRef.current?.abort();
  }, []);

  const clear = useCallback(() => {
    setMessages([]);
    setError(null);
  }, []);

  return { messages, send, stop, clear, streaming, error };
}
```

---

## src/components/ChatUI.tsx

```tsx
import { useState, useRef, useEffect } from 'react';
import { useChat } from '../hooks/useChat';

interface Props {
  port: number;
}

export function ChatUI({ port }: Props) {
  const { messages, send, stop, streaming, error } = useChat({ port });
  const [input, setInput] = useState('');
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages]);

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!input.trim() || streaming) return;
    send(input.trim());
    setInput('');
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100vh', padding: '1rem' }}>
      <div style={{ flex: 1, overflowY: 'auto', marginBottom: '1rem' }}>
        {messages.map((msg, i) => (
          <div key={i} style={{ marginBottom: '0.75rem', textAlign: msg.role === 'user' ? 'right' : 'left' }}>
            <span style={{ background: msg.role === 'user' ? '#0070f3' : '#333', color: '#fff', padding: '0.5rem 1rem', borderRadius: '1rem', display: 'inline-block', maxWidth: '80%', whiteSpace: 'pre-wrap' }}>
              {msg.content || (streaming ? '▋' : '')}
            </span>
          </div>
        ))}
        {error && <p style={{ color: 'red' }}>{error}</p>}
        <div ref={bottomRef} />
      </div>

      <form onSubmit={handleSubmit} style={{ display: 'flex', gap: '0.5rem' }}>
        <input
          value={input}
          onChange={e => setInput(e.target.value)}
          placeholder="Type a message..."
          style={{ flex: 1, padding: '0.5rem', fontSize: '1rem' }}
          disabled={streaming}
        />
        {streaming
          ? <button type="button" onClick={stop}>Stop</button>
          : <button type="submit" disabled={!input.trim()}>Send</button>
        }
      </form>
    </div>
  );
}
```

---

## src/App.tsx

```tsx
import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ChatUI } from './components/ChatUI';

const MODEL_PATH = 'C:/path/to/your/model.gguf'; // replace or make configurable

async function waitForServer(port: number, timeout = 20000): Promise<boolean> {
  const start = Date.now();
  while (Date.now() - start < timeout) {
    try {
      const r = await fetch(`http://127.0.0.1:${port}/health`);
      if (r.ok) return true;
    } catch {}
    await new Promise(res => setTimeout(res, 500));
  }
  return false;
}

export default function App() {
  const [status, setStatus] = useState<'idle' | 'starting' | 'ready' | 'error'>('idle');
  const [port, setPort] = useState<number>(8080);

  useEffect(() => {
    async function boot() {
      setStatus('starting');
      try {
        const p = await invoke<number>('start_llm_server', { modelPath: MODEL_PATH });
        setPort(p);
        const ok = await waitForServer(p);
        setStatus(ok ? 'ready' : 'error');
      } catch (e) {
        console.error(e);
        setStatus('error');
      }
    }
    boot();
  }, []);

  if (status === 'starting') return <p style={{ padding: '2rem' }}>Loading model...</p>;
  if (status === 'error') return <p style={{ padding: '2rem', color: 'red' }}>Failed to start llama-server. Check model path and binary.</p>;
  if (status !== 'ready') return null;

  return <ChatUI port={port} />;
}
```

---

## Windows-Specific Gotchas

### 1. Binary paths with spaces
Always use `PathBuf`, never string concatenation. Windows paths with spaces (`C:\Program Files\...`) break `Command::new` if passed as a raw string.

### 2. CORS header required
llama-server doesn't add CORS headers by default. Pass `--cors-allow-origin '*'` when spawning. Without it, `fetch` from the Tauri renderer returns a CORS error (even though it's localhost).

### 3. Port conflicts
8080 is commonly used. Check before starting:

```rust
use std::net::TcpListener;

fn find_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
```

### 4. Server warmup time
llama-server takes 2–15s to load a model. Always poll `/health` before enabling the UI. Never just `sleep` a fixed duration.

### 5. Process leak on crash
If the Tauri app crashes, llama-server keeps running. The `Drop` impl on `LlmServer` handles clean exits, but for crash recovery, check on startup whether a server is already on the target port:

```rust
// In start(): if /health returns 200, reuse the existing process
```

### 6. Bundling the binary (production)
- Add `resources/llama-server.exe` to `tauri.conf.json` bundle resources.
- In Rust, retrieve with `app.path().resource_dir()?.join("resources/llama-server.exe")`.
- NSIS installer on Windows may need `allowDangerousDefaultPermissions` or explicit allowlisting for the binary — check Tauri v2 NSIS docs.

### 7. `windows_subsystem = "windows"` hides console
In release builds, `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` suppresses the console window. This also suppresses llama-server's stdout. If you want to capture logs, pipe `stdout`/`stderr` on `Command::spawn`.

---

## Dev Workflow

```bash
# Terminal 1: Vite dev server
npm run dev

# Terminal 2: Tauri dev (spawns Rust + Electron-like window)
cargo tauri dev
```

In dev mode, `get_binary_path()` returns `"llama-server.exe"` — ensure it's on `PATH` or hardcode a local path temporarily.

---

## Build for Production

```bash
cargo tauri build
# Output: src-tauri/target/release/bundle/nsis/my-tauri-llm_0.1.0_x64-setup.exe
```

Place `llama-server.exe` in `resources/` before building so it gets bundled.

---

## Minimal Test Checklist (for agent)

- [ ] `cargo tauri dev` boots without Rust compile errors
- [ ] Sending a message returns a streamed response
- [ ] Closing the window kills the llama-server process (`tasklist` check)
- [ ] `cargo tauri build` produces a working `.exe` installer
- [ ] Installed app loads model and responds within 20s on first run
