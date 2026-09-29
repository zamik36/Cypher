import { render } from "solid-js/web";
import App from "@cypher/ui/App";
import { registerPlatform } from "@cypher/ui/platform";
import "@cypher/ui/index.css";
import "./index.css";
import InstallPrompt from "./InstallPrompt";
import { webPlatform } from "./web/platform";

registerPlatform(webPlatform);

if ("serviceWorker" in navigator) {
  navigator.serviceWorker.register("/sw.js").catch(() => undefined);
}

const root = document.getElementById("root");
if (!root) throw new Error("index.html has no #root");
render(
  () => (
    <>
      <App />
      <InstallPrompt />
    </>
  ),
  root,
);
