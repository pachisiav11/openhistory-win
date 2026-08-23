/**
 * Everything the user can change, in the order it matters.
 *
 * Recording first, then who writes the summaries, then what other tools may ask, then
 * the data itself. The summary section is one dropdown of every model rather than a
 * provider list and a model list: the question being answered is which model writes
 * the summaries, and the provider follows from the answer.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  apiKeys,
  cancelDownload,
  cloudModels,
  deleteAllHistory,
  downloadModel,
  forgetApiKey,
  forgetMcpTokens,
  getConfig,
  inferenceReadiness,
  localModels,
  localServerStatus,
  mcpClientConfig,
  mcpStatus,
  onDownload,
  regenerateMcpToken,
  removeModel,
  setConfig,
  startMcp,
  stopLocalServer,
  stopMcp,
  storeApiKey,
  useCloudModel,
  useLocalModel,
  type CloudModel,
  type Config,
  type DownloadProgress,
  type InferenceProvider,
  type KeyStatus,
  type LlamaStatus,
  type LocalModel,
  type McpStatus,
  type Readiness,
} from "../lib/ipc";

interface Props {
  /** Called when a change may have moved what the other views show. */
  onChanged: () => void;
}

/** The value of the model dropdown when nothing hosted is chosen. */
const DISABLED = "disabled";
const LOCAL_PREFIX = "local:";

function megabytes(bytes: number): string {
  const gb = bytes / 1_000_000_000;
  return gb >= 1 ? `${gb.toFixed(1)} GB` : `${Math.round(bytes / 1_000_000)} MB`;
}

function percent(progress: DownloadProgress): number | null {
  if (!progress.totalBytes) return null;
  return Math.min(100, Math.round((progress.downloadedBytes / progress.totalBytes) * 100));
}

export default function Settings({ onChanged }: Props) {
  const [config, setLocalConfig] = useState<Config | null>(null);
  const [cloud, setCloud] = useState<CloudModel[]>([]);
  const [models, setModels] = useState<LocalModel[]>([]);
  const [keys, setKeys] = useState<KeyStatus[]>([]);
  const [readiness, setReadiness] = useState<Readiness | null>(null);
  const [llama, setLlama] = useState<LlamaStatus | null>(null);
  const [mcp, setMcp] = useState<McpStatus | null>(null);
  const [token, setToken] = useState<string | null>(null);
  const [snippet, setSnippet] = useState<string | null>(null);
  const [downloads, setDownloads] = useState<Record<string, DownloadProgress>>({});
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [excluded, setExcluded] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const refresh = useCallback(() => {
    getConfig()
      .then((loaded) => {
        setLocalConfig(loaded);
        setExcluded(loaded.recording.excludedApps.join(", "));
      })
      .catch(setError);
    cloudModels().then(setCloud).catch(setError);
    localModels().then(setModels).catch(setError);
    apiKeys().then(setKeys).catch(setError);
    inferenceReadiness().then(setReadiness).catch(setError);
    localServerStatus().then(setLlama).catch(setError);
    mcpStatus().then(setMcp).catch(setError);
  }, []);

  useEffect(refresh, [refresh]);

  useEffect(
    () =>
      onDownload((progress) => {
        setDownloads((all) => ({ ...all, [progress.modelId]: progress }));
        if (progress.done) {
          localModels().then(setModels).catch(setError);
        }
      }),
    [],
  );

  /** Run one change, then reload everything it could have moved. */
  const act = useCallback(
    async (work: () => Promise<unknown>, said?: string) => {
      setBusy(true);
      setError(null);
      setNote(null);
      try {
        await work();
        refresh();
        onChanged();
        if (said) setNote(said);
      } catch (cause) {
        setError(cause instanceof Error ? cause.message : String(cause));
      } finally {
        setBusy(false);
      }
    },
    [refresh, onChanged],
  );

  /**
   * The newest configuration, so two changes made before the first round trip
   * returns both land. A change built from the render's `config` would carry the
   * earlier field back to its old value.
   */
  const latest = useRef<Config | null>(null);
  useEffect(() => {
    latest.current = config;
  }, [config]);

  const save = useCallback(
    (change: (current: Config) => Config, said?: string) =>
      act(async () => {
        const next = change(latest.current!);
        latest.current = next;
        setLocalConfig(next);
        await setConfig(next);
      }, said),
    [act],
  );

  const byVendor = useMemo(() => {
    const groups = new Map<string, CloudModel[]>();
    for (const model of cloud) {
      const existing = groups.get(model.vendor);
      if (existing) existing.push(model);
      else groups.set(model.vendor, [model]);
    }
    return [...groups.entries()];
  }, [cloud]);

  if (!config) {
    return <p className="empty">Loading settings…</p>;
  }

  const provider = config.inference.provider;
  const selection =
    provider === "local"
      ? `${LOCAL_PREFIX}${config.inference.localModelId ?? ""}`
      : provider === "disabled"
        ? DISABLED
        : config.inference.cloudModel;

  const chosen = cloud.find((one) => one.id === config.inference.cloudModel);
  const needsKey = provider !== "disabled" && provider !== "local" && chosen && !chosen.hasKey;

  const chooseModel = (value: string) => {
    if (value === DISABLED) {
      save((current) => ({
        ...current,
        inference: { ...current.inference, provider: "disabled" },
      }));
    } else if (value.startsWith(LOCAL_PREFIX)) {
      act(() => useLocalModel(value.slice(LOCAL_PREFIX.length)));
    } else {
      act(() => useCloudModel(value));
    }
  };

  return (
    <section aria-label="Settings" className="settings">
      {error ? (
        <p className="notice notice--error" role="alert">
          {error}
        </p>
      ) : null}
      {note ? <p className="notice notice--ok">{note}</p> : null}

      <section className="panel" aria-label="Recording">
        <h3 className="panel__title">Recording</h3>

        <div className="records">
          <div className="records__half">
            <p className="records__head">What it records</p>
            <ul className="records__list">
              <li>The application in front of you, and the title of its window.</li>
              <li>The address of the page in a browser, without the query string.</li>
              <li>The name of the document or file a window is on.</li>
              <li>A little of the text a window is showing, with digits and anything that looks like a secret taken out.</li>
              <li>When the screen locked, slept and woke.</li>
            </ul>
          </div>
          <div className="records__half">
            <p className="records__head">What it never records</p>
            <ul className="records__list">
              <li>Screenshots, the screen itself, the camera, the microphone, or any audio.</li>
              <li>Key presses, clicks, or where the pointer went.</li>
              <li>The clipboard, or the contents of a file.</li>
              <li>Anything at all while a password field has the focus.</li>
              <li>Anything from a private browser window, or from an application on the never-recorded list below.</li>
            </ul>
          </div>
        </div>
        <p className="panel__hint">
          Time with nothing happening is worked out from the gaps between the events above.
          Nothing watches the keyboard or the pointer to measure it. All of it is written to
          this machine, and only a reduced description leaves it, and only if you choose a cloud
          model for summaries below.
        </p>

        <label className="field field--check">
          <input
            type="checkbox"
            checked={config.startWithWindows}
            onChange={(event) =>
              save((current) => ({ ...current, startWithWindows: event.target.checked }))
            }
          />
          <span>
            Start OpenHistory when I sign in to Windows. Started that way it goes straight to the
            tray, without opening a window.
          </span>
        </label>

        <label className="field field--check">
          <input
            type="checkbox"
            checked={config.startOnLaunch}
            onChange={(event) =>
              save((current) => ({ ...current, startOnLaunch: event.target.checked }))
            }
          />
          <span>Start recording when the application launches</span>
        </label>

        <label className="field field--check">
          <input
            type="checkbox"
            checked={config.recording.captureUrls}
            onChange={(event) =>
              save((current) => ({
                ...current,
                recording: { ...current.recording, captureUrls: event.target.checked },
              }))
            }
          />
          <span>Record the address of the page in the browser</span>
        </label>

        <label className="field field--check">
          <input
            type="checkbox"
            checked={config.recording.captureDocuments}
            onChange={(event) =>
              save((current) => ({
                ...current,
                recording: { ...current.recording, captureDocuments: event.target.checked },
              }))
            }
          />
          <span>
            Record the document a window is on: the name of the spreadsheet, never a cell of it
          </span>
        </label>

        <label className="field field--check">
          <input
            type="checkbox"
            checked={config.recording.captureVisibleText}
            onChange={(event) =>
              save((current) => ({
                ...current,
                recording: { ...current.recording, captureVisibleText: event.target.checked },
              }))
            }
          />
          <span>
            Record a little of the text a window is showing. At most a dozen short lines, read
            once every thirty seconds, with password fields, long runs of digits and anything
            that looks like a key or a token removed before it is written
          </span>
        </label>

        <label className="field">
          <span className="field__label">Applications never recorded</span>
          <input
            type="text"
            className="input"
            value={excluded}
            placeholder="1password, bitwarden, keepassxc"
            onChange={(event) => setExcluded(event.target.value)}
            onBlur={() =>
              save((current) => ({
                ...current,
                recording: {
                  ...current.recording,
                  excludedApps: excluded
                    .split(",")
                    .map((one) => one.trim().toLowerCase())
                    .filter(Boolean),
                },
              }))
            }
          />
          <span className="field__hint">
            Matched against the application name. The list is read when recording starts, so a
            change takes effect on the next start.
          </span>
        </label>

        <label className="field">
          <span className="field__label">Days of history to keep</span>
          <input
            type="number"
            className="input input--number"
            min={0}
            value={config.retentionDays}
            onChange={(event) =>
              save((current) => ({
                ...current,
                retentionDays: Math.max(0, Number(event.target.value) || 0),
              }))
            }
          />
          <span className="field__hint">Zero keeps everything.</span>
        </label>
      </section>

      <section className="panel" aria-label="Summaries">
        <h3 className="panel__title">Summaries</h3>

        <label className="field">
          <span className="field__label">Model</span>
          <select
            className="input"
            value={selection}
            disabled={busy}
            onChange={(event) => chooseModel(event.target.value)}
          >
            <option value={DISABLED}>No summaries</option>
            {byVendor.map(([vendor, entries]) => (
              <optgroup key={vendor} label={vendor}>
                {entries.map((model) => (
                  <option key={model.id} value={model.id}>
                    {model.name}
                    {model.hasKey ? "" : " — needs a key"}
                  </option>
                ))}
              </optgroup>
            ))}
            {models.some((one) => one.installed) ? (
              <optgroup label="On this machine">
                {models
                  .filter((one) => one.installed)
                  .map((one) => (
                    <option key={one.id} value={`${LOCAL_PREFIX}${one.id}`}>
                      {one.name}
                    </option>
                  ))}
              </optgroup>
            ) : null}
          </select>
          {chosen && provider !== "local" && provider !== "disabled" ? (
            <span className="field__hint">{chosen.note}</span>
          ) : null}
        </label>

        <label className="field field--check">
          <input
            type="checkbox"
            checked={config.inference.cloudConsent}
            onChange={(event) =>
              save((current) => ({
                ...current,
                inference: { ...current.inference, cloudConsent: event.target.checked },
              }))
            }
          />
          <span>
            Send a reduced description of each day to{" "}
            {provider !== "disabled" && provider !== "local" && chosen
              ? chosen.vendor
              : "a cloud provider"}
            . That description carries application names, window titles, the names of the
            documents you were on and a few lines of the text those windows were showing.
            Private sessions become an application and a span of time, addresses lose their
            query strings, and no file path ever leaves this machine. Nothing is sent while the
            model above is “No summaries” or one on this machine.
          </span>
        </label>

        <label className="field field--check">
          <input
            type="checkbox"
            checked={config.inference.autoSummarize}
            onChange={(event) =>
              save((current) => ({
                ...current,
                inference: { ...current.inference, autoSummarize: event.target.checked },
              }))
            }
          />
          <span>Write summaries as the day fills in, rather than only when asked</span>
        </label>

        {readiness ? (
          <p className={`panel__hint${readiness.ready ? " panel__hint--ok" : ""}`}>
            {readiness.ready
              ? `Ready. ${readiness.model ?? readiness.provider} will write the summaries.`
              : (readiness.blockedBy ?? "Summaries are not configured.")}
          </p>
        ) : null}

        <h4 className="panel__sub">API keys</h4>
        {needsKey ? (
          <p className="panel__hint">The model chosen above has no key stored yet.</p>
        ) : null}
        <ul className="keys">
          {keys.map((key) => (
            <li key={key.provider} className="key">
              <label className="field">
                <span className="field__label">
                  {key.label}
                  {key.stored ? " · stored" : ""}
                </span>
                <span className="key__row">
                  <input
                    type="password"
                    className="input"
                    autoComplete="off"
                    placeholder={key.stored ? "••••••••  (replace)" : "Paste the key"}
                    value={drafts[key.provider] ?? ""}
                    onChange={(event) =>
                      setDrafts((all) => ({ ...all, [key.provider]: event.target.value }))
                    }
                  />
                  <button
                    type="button"
                    className="button"
                    disabled={busy || !(drafts[key.provider] ?? "").trim()}
                    onClick={() =>
                      act(async () => {
                        await storeApiKey(
                          key.provider as InferenceProvider,
                          (drafts[key.provider] ?? "").trim(),
                        );
                        setDrafts((all) => ({ ...all, [key.provider]: "" }));
                      }, `${key.label} saved.`)
                    }
                  >
                    Save
                  </button>
                  {key.stored ? (
                    <button
                      type="button"
                      className="button button--quiet"
                      disabled={busy}
                      onClick={() =>
                        act(
                          () => forgetApiKey(key.provider as InferenceProvider),
                          `${key.label} removed.`,
                        )
                      }
                    >
                      Forget
                    </button>
                  ) : null}
                </span>
              </label>
            </li>
          ))}
        </ul>
        <p className="field__hint">
          Keys are kept in the Windows Credential Manager, one entry per provider. A stored key is
          never sent back to this window.
        </p>

        <h4 className="panel__sub">Models on this machine</h4>
        <ul className="models">
          {models.map((model) => {
            const progress = downloads[model.id];
            const running = progress !== undefined && !progress.done;
            const done = progress ? percent(progress) : null;
            return (
              <li key={model.id} className="model">
                <div className="model__body">
                  <span className="model__name">
                    {model.name}
                    {model.installed ? " · installed" : ""}
                  </span>
                  <span className="model__detail">
                    {model.parameters} · {model.quantization} · {megabytes(model.approximateBytes)}
                    {model.fitsMemory ? "" : " · more memory recommended"}
                  </span>
                  <span className="model__note">{model.note}</span>
                  {running ? (
                    <span className="model__progress">
                      <span
                        className="model__fill"
                        style={{ width: done === null ? "100%" : `${done}%` }}
                      />
                    </span>
                  ) : null}
                  {progress?.error ? (
                    <span className="model__error">{progress.error}</span>
                  ) : null}
                </div>

                <div className="model__actions">
                  {running ? (
                    <button
                      type="button"
                      className="button button--quiet"
                      onClick={() => act(() => cancelDownload(model.id))}
                    >
                      Cancel
                    </button>
                  ) : model.installed ? (
                    <button
                      type="button"
                      className="button button--quiet"
                      disabled={busy}
                      onClick={() => act(() => removeModel(model.id))}
                    >
                      Delete
                    </button>
                  ) : (
                    <button
                      type="button"
                      className="button"
                      disabled={busy}
                      onClick={() => act(() => downloadModel(model.id))}
                    >
                      Download
                    </button>
                  )}
                </div>
              </li>
            );
          })}
        </ul>

        {llama?.running ? (
          <p className="panel__hint">
            The local model server is running on port {llama.port}
            {llama.model ? ` with ${llama.model}` : ""}.{" "}
            <button
              type="button"
              className="episode__more"
              onClick={() => act(() => stopLocalServer(), "The local model was unloaded.")}
            >
              Unload it now
            </button>
          </p>
        ) : null}
      </section>

      <section className="panel" aria-label="Local MCP server">
        <h3 className="panel__title">Local MCP server</h3>
        <p className="field__hint">
          Lets another program on this machine — Claude Code, an editor extension — ask what you
          have been working on. It listens on 127.0.0.1 only, and every request needs a token
          issued here.
        </p>

        <label className="field field--check">
          <input
            type="checkbox"
            checked={config.mcp.enabled}
            disabled={busy}
            onChange={(event) =>
              act(async () => {
                if (event.target.checked) {
                  const started = await startMcp();
                  if (started.token) setToken(started.token);
                } else {
                  await stopMcp();
                  setToken(null);
                  setSnippet(null);
                }
              })
            }
          />
          <span>Answer questions from other programs on this machine</span>
        </label>

        <label className="field field--check">
          <input
            type="checkbox"
            checked={config.mcp.allowHistory}
            disabled={busy}
            onChange={(event) =>
              save((current) => ({
                ...current,
                mcp: { ...current.mcp, allowHistory: event.target.checked },
              }))
            }
          />
          <span>Answer about earlier days, not only today</span>
        </label>

        <label className="field">
          <span className="field__label">Preferred port</span>
          <input
            type="number"
            className="input input--number"
            value={config.mcp.port}
            onChange={(event) =>
              save((current) => ({
                ...current,
                mcp: { ...current.mcp, port: Number(event.target.value) || current.mcp.port },
              }))
            }
          />
          <span className="field__hint">
            {mcp?.running
              ? `Listening on ${mcp.url}.`
              : "Not running. If the port is taken, a free one is used instead."}
          </span>
        </label>

        <div className="panel__actions">
          <button
            type="button"
            className="button"
            disabled={busy}
            onClick={() =>
              act(async () => {
                setToken(await regenerateMcpToken());
                setSnippet(null);
              }, "A new token was issued. Every earlier one has stopped working.")
            }
          >
            {mcp?.hasToken ? "Regenerate token" : "Issue a token"}
          </button>
          {mcp?.hasToken ? (
            <button
              type="button"
              className="button button--quiet"
              disabled={busy}
              onClick={() =>
                act(async () => {
                  await forgetMcpTokens();
                  setToken(null);
                  setSnippet(null);
                }, "Every token was discarded.")
              }
            >
              Forget tokens
            </button>
          ) : null}
          {mcp?.running ? (
            <button
              type="button"
              className="button button--quiet"
              disabled={busy}
              onClick={() => act(async () => setSnippet(await mcpClientConfig(token ?? undefined)))}
            >
              Show client configuration
            </button>
          ) : null}
        </div>

        {token ? (
          <div className="token">
            <p className="token__label">
              Copy this now. Only its fingerprint is stored, so it cannot be shown again.
            </p>
            <code className="token__value">{token}</code>
          </div>
        ) : null}

        {snippet ? <pre className="snippet">{snippet}</pre> : null}
      </section>

      <section className="panel panel--danger" aria-label="Data">
        <h3 className="panel__title">Data</h3>
        <p className="field__hint">
          Everything is kept on this machine, in files you can read. Deleting the history removes
          the event log, every processed day, the search index, and every summary. It cannot be
          undone.
        </p>
        <button
          type="button"
          className="button button--danger"
          disabled={busy}
          onClick={() => {
            if (!window.confirm("Delete all recorded history? This cannot be undone.")) return;
            act(async () => {
              const gone = await deleteAllHistory();
              setNote(
                `Deleted ${gone.days} day${gone.days === 1 ? "" : "s"} of history and ${gone.summaries} summar${
                  gone.summaries === 1 ? "y" : "ies"
                }.`,
              );
            });
          }}
        >
          Delete all history
        </button>
      </section>
    </section>
  );
}
