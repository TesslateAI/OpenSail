import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App.tsx";
import { installVoieDocumentTitle } from "./document-title.ts";
import "./styles.css";
export { mountDshApp, unmountDshApp } from "./connection-voie/adapter.ts";

installVoieDocumentTitle();

const root = document.getElementById("root");
if (root === null) throw new Error("voie console: missing #root");

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

