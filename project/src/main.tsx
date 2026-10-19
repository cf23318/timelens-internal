import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { DesktopPageBoundary } from "./components/DesktopPageBoundary";
import "./index.css";
import "./i18n";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <DesktopPageBoundary><App /></DesktopPageBoundary>
  </React.StrictMode>
);
