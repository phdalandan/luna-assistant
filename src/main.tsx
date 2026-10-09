import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { showScrollbarsWhileScrolling } from "./lib/scrollbars";
import "./styles.css";

const root = document.getElementById("root");
if (!root) {
  throw new Error("Root element not found");
}

showScrollbarsWhileScrolling();

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
