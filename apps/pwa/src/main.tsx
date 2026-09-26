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

render(
  () => (
    <>
      <App />
      <InstallPrompt />
    </>
  ),
  document.getElementById("root")!,
);
