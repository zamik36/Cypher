import { For } from "solid-js";
import "./Overlays.css";
import Icon from "./Icon";
import { toasts, removeToast } from "../stores/toasts";
import { t } from "../i18n";

export default function ToastContainer() {
  return (
    <div class="toasts" role="status" aria-live="polite">
      <For each={toasts}>
        {(toast) => (
          <div class="toast" data-type={toast.type}>
            <Icon name={toast.type === "success" ? "check" : toast.type === "error" ? "alert" : "info"} size={18} />
            <span class="toast__text">{toast.message}</span>
            <button class="toast__close" aria-label={t().common_close} onClick={() => removeToast(toast.id)}>
              <Icon name="x" size={16} />
            </button>
          </div>
        )}
      </For>
    </div>
  );
}
