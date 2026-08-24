import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import Settings from "./Settings";
import {
  clearMocks,
  emitDownload,
  mockCommand,
  type Config,
  type McpHandle,
  type McpStatus,
} from "../lib/ipc";
import { backend, cloudModel, config, keys, localModel } from "../test/fixtures";

afterEach(() => {
  clearMocks();
  vi.restoreAllMocks();
});

/** Every cloud model the backend offers, across the three providers. */
function sevenModels() {
  return [
    cloudModel({ id: "claude-haiku-4-5", name: "Claude Haiku (latest)" }),
    cloudModel({ id: "claude-sonnet-5", name: "Claude Sonnet (latest)" }),
    cloudModel({ id: "claude-opus-5", name: "Claude Opus (latest)" }),
    cloudModel({
      id: "gpt-5.6-luna",
      name: "GPT-5.6 Luna",
      provider: "openai",
      vendor: "OpenAI",
    }),
    cloudModel({
      id: "gpt-5.6-terra",
      name: "GPT-5.6 Terra",
      provider: "openai",
      vendor: "OpenAI",
    }),
    cloudModel({ id: "gpt-5.6-sol", name: "GPT-5.6 Sol", provider: "openai", vendor: "OpenAI" }),
    cloudModel({
      id: "gemini-flash-latest",
      name: "Gemini Flash (latest)",
      provider: "google",
      vendor: "Google AI Studio",
    }),
  ];
}

describe("Settings — summaries", () => {
  it("lets the user agree to cloud summaries before any model is chosen", async () => {
    // Consent used to appear only once a cloud model was selected, which left a
    // user who had chosen nothing with no way to agree at all.
    backend();
    const saved: Config[] = [];
    mockCommand("set_config", (args) => {
      saved.push(args?.config as Config);
      return args?.config as Config;
    });
    mockCommand("cloud_models", sevenModels);
    render(<Settings onChanged={() => {}} />);

    const agree = await screen.findByRole("checkbox", { name: /I agree to send a reduced description/ });
    expect(agree).not.toBeChecked();
    fireEvent.click(agree);

    await waitFor(() => expect(saved).toHaveLength(1));
    expect(saved[0]!.inference.cloudConsent).toBe(true);
    expect(saved[0]!.inference.provider).toBe("disabled");
  });

  it("asks where summaries are written before asking which model", async () => {
    backend();
    mockCommand("cloud_models", sevenModels);
    render(<Settings onChanged={() => {}} />);

    expect(await screen.findByRole("radio", { name: /No summaries/ })).toBeChecked();
    expect(screen.getByRole("radio", { name: /On this machine/ })).not.toBeChecked();
    expect(screen.getByRole("radio", { name: /A cloud provider/ })).not.toBeChecked();

    // Neither model list is on the page until a place has been chosen.
    expect(screen.queryByRole("combobox")).not.toBeInTheDocument();
  });

  it("offers every cloud model grouped by who runs it, once the cloud is chosen", async () => {
    backend({
      config: config({
        inference: { ...config().inference, provider: "anthropic" },
      }),
    });
    mockCommand("cloud_models", sevenModels);
    render(<Settings onChanged={() => {}} />);

    const dropdown = await screen.findByRole("combobox");
    const options = within(dropdown).getAllByRole("option");
    // The seven models, and no off switch: that is the radio's job now.
    expect(options).toHaveLength(7);
    expect(options.map((one) => one.textContent)).toContain("Gemini Flash (latest) — needs a key");

    const groups = dropdown.querySelectorAll("optgroup");
    expect([...groups].map((one) => one.label)).toEqual([
      "Anthropic",
      "OpenAI",
      "Google AI Studio",
    ]);
  });

  it("lets the user pick which downloaded model is the default", async () => {
    backend({
      config: config({
        inference: { ...config().inference, provider: "local", localModelId: "one" },
      }),
    });
    mockCommand("local_models", () => [
      localModel({ id: "one", name: "Model One", installed: true }),
      localModel({ id: "two", name: "Model Two", installed: true }),
      localModel({ id: "three", name: "Model Three", installed: false }),
    ]);
    const chosen: string[] = [];
    mockCommand("use_local_model", (args): Config => {
      chosen.push(String(args?.id));
      return config({
        inference: {
          ...config().inference,
          provider: "local",
          localModelId: String(args?.id),
        },
      });
    });
    render(<Settings onChanged={() => {}} />);

    const dropdown = await screen.findByRole("combobox");
    // Only what is on disk can be the default.
    expect(within(dropdown).getAllByRole("option").map((one) => one.textContent)).toEqual([
      "Model One",
      "Model Two",
    ]);

    await userEvent.setup().selectOptions(dropdown, "two");
    await waitFor(() => expect(chosen).toEqual(["two"]));
  });

  it("says llama-server is not on this machine yet, and offers to fetch it", async () => {
    backend({
      config: config({
        inference: { ...config().inference, provider: "local", localModelId: "one" },
      }),
    });
    mockCommand("local_models", () => [localModel({ id: "one", installed: true })]);
    render(<Settings onChanged={() => {}} />);

    expect(await screen.findByDisplayValue("Not on this machine yet")).toBeInTheDocument();
    expect(screen.getByText(/fetched for you the first time/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Find/ })).toBeEnabled();
    expect(screen.getByRole("button", { name: /Get it/ })).toBeEnabled();
  });

  it("fetches llama-server on request and shows where it landed", async () => {
    backend({
      config: config({
        inference: { ...config().inference, provider: "local", localModelId: "one" },
      }),
    });
    mockCommand("local_models", () => [localModel({ id: "one", installed: true })]);
    render(<Settings onChanged={() => {}} />);

    await screen.findByDisplayValue("Not on this machine yet");
    await userEvent.setup().click(screen.getByRole("button", { name: /Get it/ }));

    expect(await screen.findByDisplayValue(/llama-server\.exe$/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Get it/ })).not.toBeInTheDocument();
  });

  it("sets the provider from the model, not from a second dropdown", async () => {
    backend({
      config: config({
        inference: { ...config().inference, provider: "anthropic" },
      }),
    });
    mockCommand("cloud_models", sevenModels);
    const chosen: string[] = [];
    mockCommand("use_cloud_model", (args): Config => {
      chosen.push(String(args?.id));
      return config({
        inference: { ...config().inference, provider: "google", cloudModel: String(args?.id) },
      });
    });
    render(<Settings onChanged={() => {}} />);

    const user = userEvent.setup();
    await user.selectOptions(await screen.findByRole("combobox"), "gemini-flash-latest");

    await waitFor(() => expect(chosen).toEqual(["gemini-flash-latest"]));
  });

  it("asks for consent before anything can be sent, and says what is sent", async () => {
    backend({
      config: config({
        inference: { ...config().inference, provider: "anthropic", cloudModel: "claude-haiku-4-5" },
      }),
      readiness: {
        provider: "anthropic",
        ready: false,
        blockedBy: "Cloud summaries need your agreement before anything is sent.",
      },
    });
    mockCommand("cloud_models", sevenModels);
    render(<Settings onChanged={() => {}} />);

    expect(await screen.findByText(/Private sessions become an application/)).toBeInTheDocument();
    expect(screen.getByText(/need your agreement/)).toBeInTheDocument();
  });

  it("offers consent with no model chosen, and says nothing is sent until one is", async () => {
    backend();
    render(<Settings onChanged={() => {}} />);

    await screen.findByRole("radio", { name: /No summaries/ });
    expect(screen.getByRole("checkbox", { name: /I agree to send a reduced description/ })).toBeEnabled();
    expect(screen.getByText(/a cloud provider/)).toBeInTheDocument();
    expect(screen.getByText(/Nothing is sent while the model above is/)).toBeInTheDocument();
  });

  it("has a field for each provider's key and never shows a stored one", async () => {
    backend();
    mockCommand("api_keys", () => keys(["anthropic"]));
    render(<Settings onChanged={() => {}} />);

    expect(await screen.findByText(/Anthropic API key · stored/)).toBeInTheDocument();
    expect(screen.getByText("OpenAI API key")).toBeInTheDocument();
    expect(screen.getByText("Google AI Studio API key")).toBeInTheDocument();

    const stored = screen.getByPlaceholderText(/replace/);
    expect(stored).toHaveValue("");
    expect(stored).toHaveAttribute("type", "password");
  });

  it("stores a key and clears the field", async () => {
    backend();
    const saved: Array<[string, string]> = [];
    mockCommand("store_api_key", (args) => {
      saved.push([String(args?.provider), String(args?.key)]);
      return true;
    });
    render(<Settings onChanged={() => {}} />);

    const user = userEvent.setup();
    const fields = await screen.findAllByPlaceholderText("Paste the key");
    await user.type(fields[0]!, "sk-ant-secret");
    await user.click(screen.getAllByRole("button", { name: "Save" })[0]!);

    await waitFor(() => expect(saved).toEqual([["anthropic", "sk-ant-secret"]]));
    expect(fields[0]).toHaveValue("");
  });
});

describe("Settings — local models", () => {
  it("shows a download progressing and then installed", async () => {
    let installed = false;
    backend();
    mockCommand("local_models", () => [localModel({ installed })]);
    mockCommand("download_model", () => localModel({ installed: false }));
    render(<Settings onChanged={() => {}} />);

    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Download" }));

    emitDownload({
      modelId: "qwen2.5-3b-instruct-q4",
      downloadedBytes: 500_000_000,
      totalBytes: 2_100_000_000,
      done: false,
    });
    expect(await screen.findByRole("button", { name: "Cancel" })).toBeInTheDocument();

    installed = true;
    emitDownload({
      modelId: "qwen2.5-3b-instruct-q4",
      downloadedBytes: 2_100_000_000,
      totalBytes: 2_100_000_000,
      done: true,
    });
    expect(await screen.findByText(/Qwen2.5 3B Instruct · installed/)).toBeInTheDocument();
  });

  it("says why a download stopped", async () => {
    backend();
    render(<Settings onChanged={() => {}} />);
    await screen.findByRole("button", { name: "Download" });

    emitDownload({
      modelId: "qwen2.5-3b-instruct-q4",
      downloadedBytes: 0,
      done: true,
      error: "The download was cancelled.",
    });

    expect(await screen.findByText("The download was cancelled.")).toBeInTheDocument();
  });
});

describe("Settings — the MCP server", () => {
  it("is off, and turning it on shows a token exactly once", async () => {
    let mcp: McpStatus = { running: false, hasToken: false };
    backend({ mcp });
    mockCommand("mcp_status", () => mcp);
    mockCommand("start_mcp", (): McpHandle => {
      mcp = { running: true, port: 47123, url: "http://127.0.0.1:47123", hasToken: true };
      return { ...mcp, token: "oh_deadbeef" };
    });
    render(<Settings onChanged={() => {}} />);

    const user = userEvent.setup();
    await user.click(
      await screen.findByRole("checkbox", { name: /Answer questions from other programs/ }),
    );

    expect(await screen.findByText("oh_deadbeef")).toBeInTheDocument();
    expect(screen.getByText(/cannot be shown again/)).toBeInTheDocument();
  });

  it("offers a client snippet once the server is up", async () => {
    backend({ mcp: { running: true, port: 47123, url: "http://127.0.0.1:47123", hasToken: true } });
    mockCommand("mcp_client_config", () => '{\n  "mcpServers": {}\n}');
    render(<Settings onChanged={() => {}} />);

    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Show client configuration" }));

    expect(await screen.findByText(/"mcpServers"/)).toBeInTheDocument();
  });

  it("regenerates a token and warns that the old one is dead", async () => {
    backend({ mcp: { running: true, port: 47123, url: "http://127.0.0.1:47123", hasToken: true } });
    mockCommand("regenerate_mcp_token", () => "oh_freshtoken");
    render(<Settings onChanged={() => {}} />);

    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Regenerate token" }));

    expect(await screen.findByText("oh_freshtoken")).toBeInTheDocument();
    expect(screen.getByText(/Every earlier one has stopped working/)).toBeInTheDocument();
  });
});

describe("Settings — recording and data", () => {
  it("starts with Windows out of the box, and saves the change when turned off", async () => {
    backend();
    const saved: Config[] = [];
    mockCommand("set_config", (args) => {
      saved.push(args?.config as Config);
      return args?.config as Config;
    });
    render(<Settings onChanged={() => {}} />);

    const toggle = await screen.findByRole("checkbox", {
      name: /Start OpenHistory when I sign in to Windows/,
    });
    expect(toggle).toBeChecked();

    fireEvent.click(toggle);

    await waitFor(() => expect(saved).toHaveLength(1));
    expect(saved[0]!.startWithWindows).toBe(false);
    // The two launch settings are separate: one is whether Windows opens the
    // application, the other is whether it records once open.
    expect(saved[0]!.startOnLaunch).toBe(true);
  });

  it("saves the exclusion list as names, not as one string", async () => {
    backend();
    const saved: Config[] = [];
    mockCommand("set_config", (args) => {
      saved.push(args?.config as Config);
      return args?.config as Config;
    });
    render(<Settings onChanged={() => {}} />);

    const user = userEvent.setup();
    const field = await screen.findByDisplayValue("1password");
    await user.clear(field);
    await user.type(field, "1Password, KeePassXC ");
    await user.tab();

    await waitFor(() => expect(saved).toHaveLength(1));
    expect(saved[0]!.recording.excludedApps).toEqual(["1password", "keepassxc"]);
  });

  it("states what is recorded and what never is, before any switch", async () => {
    backend();
    render(<Settings onChanged={() => {}} />);

    expect(await screen.findByText("What it records")).toBeInTheDocument();
    expect(screen.getByText("What it never records")).toBeInTheDocument();
    expect(screen.getByText(/Key presses, clicks, or where the pointer went/)).toBeInTheDocument();
    expect(screen.getByText(/Screenshots, the screen itself/)).toBeInTheDocument();
  });

  it("turns the document and on-screen text off one at a time", async () => {
    backend();
    const saved: Config[] = [];
    mockCommand("set_config", (args) => {
      saved.push(args?.config as Config);
      return args?.config as Config;
    });
    render(<Settings onChanged={() => {}} />);

    const user = userEvent.setup();
    await user.click(await screen.findByRole("checkbox", { name: /Record the document/ }));
    await waitFor(() => expect(saved).toHaveLength(1));
    expect(saved[0]!.recording.captureDocuments).toBe(false);
    expect(saved[0]!.recording.captureVisibleText).toBe(true);

    await user.click(screen.getByRole("checkbox", { name: /Record a little of the text/ }));
    await waitFor(() => expect(saved).toHaveLength(2));
    expect(saved[1]!.recording.captureVisibleText).toBe(false);
  });

  it("keeps an earlier change when a second is made before the first lands", async () => {
    backend();
    const saved: Config[] = [];
    let release = () => {};
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    mockCommand("set_config", async (args) => {
      const next = args?.config as Config;
      saved.push(next);
      if (saved.length === 1) await held;
      return next;
    });
    render(<Settings onChanged={() => {}} />);

    const launch = await screen.findByRole("checkbox", {
      name: /Start recording when the application launches/,
    });
    fireEvent.click(launch);
    fireEvent.click(screen.getByRole("checkbox", { name: /Record the address/ }));
    release();

    await waitFor(() => expect(saved).toHaveLength(2));
    expect(saved[1]!.startOnLaunch).toBe(false);
    expect(saved[1]!.recording.captureUrls).toBe(false);
  });

  it("asks before deleting everything, and does nothing if refused", async () => {
    backend();
    let called = false;
    mockCommand("delete_all_history", () => {
      called = true;
      return { days: 1, summaries: 0 };
    });
    vi.spyOn(window, "confirm").mockReturnValue(false);
    render(<Settings onChanged={() => {}} />);

    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Delete all history" }));

    expect(called).toBe(false);
  });

  it("deletes everything once confirmed and says what went", async () => {
    backend();
    mockCommand("delete_all_history", () => ({ days: 3, summaries: 2 }));
    vi.spyOn(window, "confirm").mockReturnValue(true);
    render(<Settings onChanged={() => {}} />);

    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Delete all history" }));

    expect(await screen.findByText(/Deleted 3 days of history and 2 summaries/)).toBeInTheDocument();
  });
});
