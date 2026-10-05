import { createSignal, Match, Show, Switch } from "solid-js";
import Icon from "./Icon";
import { api, type TransferInfo, type UiFile } from "../platform";
import { transferOf, upsertTransfer } from "../stores/transfers";
import { toastError } from "../stores/toasts";
import { formatBytes } from "../utils/format";
import { locale, t } from "../i18n";

/** A file in the conversation: what it is, how far it got, what can be done. */
export default function FileCard(props: { file: UiFile; outgoing: boolean }) {
  const [busy, setBusy] = createSignal(false);
  const transfer = () => transferOf(props.file.file_id);
  const percent = () => Math.round((transfer()?.progress ?? 0) * 100);

  async function act(action: () => Promise<void>, after: Partial<TransferInfo>) {
    setBusy(true);
    try {
      await action();
      upsertTransfer({ ...after, file_id: props.file.file_id });
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="file-card" data-testid="file-card" data-state={transfer()?.status ?? "idle"}>
      <span class="file-card__icon">
        <Icon name="file" />
      </span>
      <span class="file-card__body">
        <span class="file-card__name" title={props.file.name}>
          {props.file.name}
        </span>
        <span class="file-card__meta">
          <Switch fallback={formatBytes(props.file.size, locale())}>
            <Match when={transfer()?.status === "active"}>
              {formatBytes(props.file.size, locale())} · {percent()}%
            </Match>
            <Match when={transfer()?.status === "complete"}>
              {formatBytes(props.file.size, locale())} · {props.outgoing ? t().file_sent : t().file_saved}
            </Match>
            <Match when={transfer()?.status === "error"}>
              <span class="error-text">{transfer()?.error ?? t().file_failed}</span>
            </Match>
          </Switch>
        </span>
        <Show when={transfer()?.status === "active"}>
          <span class="file-card__progress" aria-hidden="true">
            <span style={{ width: `${percent()}%` }} />
          </span>
        </Show>
        <Show when={!props.outgoing && transfer()?.status === "offered"}>
          <span class="file-card__actions">
            <button
              class="btn btn--primary"
              disabled={busy()}
              onClick={() => void act(() => api.acceptFile(props.file.file_id), { status: "active" })}
            >
              <Icon name="download" size={18} /> {t().file_accept}
            </button>
            <button
              class="btn btn--secondary"
              disabled={busy()}
              onClick={() =>
                void act(() => api.cancelTransfer(props.file.file_id), { status: "error", error: t().file_declined })
              }
            >
              {t().file_decline}
            </button>
          </span>
        </Show>
      </span>
    </div>
  );
}
