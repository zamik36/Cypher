import { t } from "../i18n";
import { addToast } from "../stores/toasts";

/** Copies `text`; a failure (e.g. a denied permission) is reported as a toast. */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    addToast(t().toast_copy_failed, "error");
    return false;
  }
}
