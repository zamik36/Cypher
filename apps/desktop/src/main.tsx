import { render } from "solid-js/web";
import App from "@cypher/ui/App";
import { registerPlatform } from "@cypher/ui/platform";
import "@cypher/ui/index.css";
import { tauriPlatform } from "./tauri";

registerPlatform(tauriPlatform);
const root = document.getElementById("root");
if (!root) throw new Error("index.html has no #root");
render(() => <App />, root);
