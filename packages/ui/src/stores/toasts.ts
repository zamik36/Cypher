import { createStore } from "solid-js/store";
import { reasonText } from "../utils/reasons";

export interface Toast {
  id: number;
  message: string;
  type: "info" | "success" | "error";
}

let nextId = 0;

const [toasts, setToasts] = createStore<Toast[]>([]);

export function addToast(message: string, type: Toast["type"] = "info") {
  const id = nextId++;
  setToasts((prev) => [...prev, { id, message, type }]);
  setTimeout(() => removeToast(id), 4000);
}

/** Shows a failure in the user's language (see `reasonText`). */
export function toastError(error: unknown) {
  addToast(reasonText(error), "error");
}

export function removeToast(id: number) {
  setToasts((prev) => prev.filter((t) => t.id !== id));
}

export { toasts };
