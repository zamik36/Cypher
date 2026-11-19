import { render } from "solid-js/web";
import App from "@cypher/ui/App";
import { registerPlatform } from "@cypher/ui/platform";
import { receiveInvite } from "@cypher/ui/invite";
import { startPush } from "@cypher/ui/push";
import { receiveShare } from "@cypher/ui/share";
import "@cypher/ui/index.css";
import "./index.css";
import InstallPrompt from "./InstallPrompt";
import UpdatePrompt from "./UpdatePrompt";
import { takeShared } from "./share";
import { webPlatform } from "./web/platform";

registerPlatform(webPlatform);

// An invite link (`/join#<code>`) opens New chat once the app is unlocked;
// the code leaves the address bar at once.
if (location.pathname === "/join") {
  receiveInvite(location.href);
  history.replaceState(null, "", "/");
}

// "Share → Шифр" from another app: the chat list asks where to send it; a
// shared invite link opens New chat instead.
if (location.search === "?share") {
  history.replaceState(null, "", "/");
  void takeShared()
    .then((share) => {
      if (share && !(share.files.length === 0 && receiveInvite(share.text))) receiveShare(share);
    })
    .catch((e: unknown) => console.warn("share:", e));
}

// The browser replaced the push subscription; register the new one.
if ("serviceWorker" in navigator) {
  navigator.serviceWorker.addEventListener("message", ({ data }) => {
    if (data === "PUSH_RENEW") void startPush();
  });
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
