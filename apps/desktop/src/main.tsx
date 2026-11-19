import { render } from "solid-js/web";
import { getCurrent, onOpenUrl } from "@tauri-apps/plugin-deep-link";
import App from "@cypher/ui/App";
import { registerPlatform } from "@cypher/ui/platform";
import { receiveInvite } from "@cypher/ui/invite";
import "@cypher/ui/index.css";
import { tauriPlatform } from "./tauri";

registerPlatform(tauriPlatform);

// Invite links (cypher://join/<code>, or the https link on Android) open New
// chat once the app is unlocked: the one that started the app, and any later.
void getCurrent()
  .then((urls) => urls?.forEach(receiveInvite))
  .catch(() => undefined);
void onOpenUrl((urls) => urls.forEach(receiveInvite)).catch(() => undefined);

const root = document.getElementById("root");
if (!root) throw new Error("index.html has no #root");
render(() => <App />, root);
