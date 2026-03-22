import { createStore } from "solid-js/store";
let nextId = 0;
const [toasts, setToasts] = createStore([]);
export function addToast(message, type = "info") {
    const id = nextId++;
    setToasts((prev) => [...prev, { id, message, type }]);
    setTimeout(() => removeToast(id), 4000);
}
export function removeToast(id) {
    setToasts((prev) => prev.filter((t) => t.id !== id));
}
export { toasts };
