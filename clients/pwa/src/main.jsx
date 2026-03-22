import { render } from "solid-js/web";
import App from "./App";
import "./index.css";
// Register service worker for PWA installability.
if ("serviceWorker" in navigator) {
    navigator.serviceWorker.register("/sw.js").catch(() => { });
}
const root = document.getElementById("root");
render(() => <App />, root);
