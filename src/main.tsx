import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { installBrowserMocks } from "./lib/browser-mocks";
import "./styles.css";

installBrowserMocks();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
