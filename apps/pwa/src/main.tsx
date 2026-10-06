import { render } from "solid-js/web";
import App from "@cypher/ui/App";
import { registerPlatform } from "@cypher/ui/platform";
import { receiveInvite } from "@cypher/ui/invite";
import "@cypher/ui/index.css";
import "./index.css";
import InstallPrompt from "./InstallPrompt";
import UpdatePrompt from "./UpdatePrompt";
import { webPlatform } from "./web/platform";

registerPlatform(webPlatform);

// An invite link (`/join#<code>`) opens New chat once the app is unlocked;
// the code leaves the address bar at once.
if (location.pathname === "/join") {
  receiveInvite(location.href);
  history.replaceState(null, "", "/");
}

const root = document.getElementById("root");
if (!root) throw new Error("index.html has no #root");
render(
  () => (
    <>
      <App />
      <UpdatePrompt />
      <InstallPrompt />
    </>
  ),
  root,
);
