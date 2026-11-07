import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./App";
import "./i18n";
import "./styles.css";

const root = document.getElementById("root");

if (root === null) {
  throw new Error("missing Dashboard root element");
}

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
