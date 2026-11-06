import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";
import { initializeLocale } from "./lib/i18n";

function renderApp() {
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
}

// The isolated fixture window must not call legacy locale/settings commands.
if (new URLSearchParams(window.location.search).get("window") === "service-fixture") {
  renderApp();
} else {
  // Each ordinary WebView loads its own locale before it renders, including hidden windows.
  void initializeLocale().catch((error) => console.error("Interface language initialization failed", error)).then((dispose) => {
    if (dispose) window.addEventListener("pagehide", (event) => { if (!event.persisted) dispose(); });
    renderApp();
  });
}
