import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./app/App";
import { browserPreview } from "./lib/bridge";
import { queryClient } from "./lib/queryClient";
import { usePlayer } from "./stores/player";
import { useSync } from "./stores/sync";
import "./styles.css";

if (browserPreview) document.documentElement.classList.add("browser-preview");

// Dev builds: automation (devtools /eval, scripts/smoke.sh) reads the live
// app state here — a dynamic import() would get a separate module instance.
if (import.meta.env.DEV) Object.assign(window, { __TP__: { player: usePlayer, sync: useSync, queryClient } });

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
