import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import App from "./App";
import { mockCommand, type AppInfo } from "./lib/ipc";

describe("App shell", () => {
  it("renders the phase list", () => {
    mockCommand("app_info", (): AppInfo => ({ name: "OpenHistory", version: "0.1.0", phase: 0 }));
    render(<App />);

    expect(screen.getByRole("heading", { name: "OpenHistory" })).toBeInTheDocument();
    expect(screen.getByText("Collector")).toBeInTheDocument();
    expect(screen.getAllByRole("listitem")).toHaveLength(7);
  });

  it("shows the app version once IPC resolves", async () => {
    mockCommand("app_info", (): AppInfo => ({ name: "OpenHistory", version: "9.9.9", phase: 2 }));
    render(<App />);

    expect(await screen.findByText(/v9\.9\.9/)).toBeInTheDocument();
  });

  it("surfaces an IPC failure instead of rendering a blank state", async () => {
    mockCommand("app_info", () => {
      throw new Error("collector offline");
    });
    render(<App />);

    expect(await screen.findByRole("alert")).toHaveTextContent("collector offline");
  });
});
