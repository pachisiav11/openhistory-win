import "@testing-library/jest-dom/vitest";
import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";
import { clearMocks } from "../lib/ipc";

// jsdom has no layout, so it implements no scrolling at all. A view that scrolls a
// row into sight would throw here for a reason that has nothing to do with the view.
Element.prototype.scrollIntoView = () => {};

afterEach(() => {
  cleanup();
  clearMocks();
});
