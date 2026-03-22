import { For } from "solid-js";
import { toasts, removeToast } from "../stores/toasts";
import { CheckIcon, AlertIcon, XIcon } from "./Icons";

export default function ToastContainer() {
  return (
    <div class="toast-container">
      <For each={toasts}>
        {(toast) => (
          <div class={`toast ${toast.type}`}>
            {toast.type === "success" && <CheckIcon width="16" height="16" />}
            {toast.type === "error" && <AlertIcon width="16" height="16" />}
            {toast.type === "info" && <AlertIcon width="16" height="16" />}
            <span class="toast-msg">{toast.message}</span>
            <button class="toast-close" onClick={() => removeToast(toast.id)}>
              <XIcon width="14" height="14" />
            </button>
          </div>
        )}
      </For>
    </div>
  );
}
