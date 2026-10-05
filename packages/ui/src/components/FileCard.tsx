import { createResource, createSignal, Match, Show, Switch } from "solid-js";
import Icon from "./Icon";
import { api, type TransferInfo, type UiFile } from "../platform";
import { transferOf, upsertTransfer } from "../stores/transfers";
import { toastError } from "../stores/toasts";
import { formatBytes } from "../utils/format";
import { locale, t } from "../i18n";

const IMAGE = /\.(avif|bmp|gif|heic|jpe?g|png|svg|webp)$/i;

/** A file in the conversation: what it is, how far it got, what can be done. */
export default function FileCard(props: { file: UiFile; outgoing: boolean }) {
  const [busy, setBusy] = createSignal(false);
  const transfer = () => transferOf(props.file.file_id);
  const percent = () => Math.round((transfer()?.progress ?? 0) * 100);
  // A file finished in an earlier session has no transfer here: ask whether
  // it is still on this device. Asked again when a transfer completes.
  const [stored] = createResource(
    () => (transfer()?.status ?? "unknown") as string,
    (status) => (status === "unknown" || status === "complete" ? api.fileSaved(props.file.file_id) : false),
  );
  const done = () => transfer()?.status === "complete" || (!transfer() && stored() === true);
  const image = () => props.file.mime.startsWith("image/") || IMAGE.test(props.file.name);

  async function act(action: () => Promise<void>, after?: Partial<TransferInfo>) {
    setBusy(true);
    try {
      await action();
      if (after) upsertTransfer({ ...after, file_id: props.file.file_id });
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="file-card" data-testid="file-card" data-state={done() ? "complete" : (transfer()?.status ?? "idle")}>
      <span class="file-card__icon">
        <Icon name={image() ? "image" : "file"} />
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
            <Match when={transfer()?.status === "error"}>
              <span class="error-text">{transfer()?.error ?? t().file_failed}</span>
            </Match>
            <Match when={done()}>
              {formatBytes(props.file.size, locale())} · {props.outgoing ? t().file_sent : t().file_saved}
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
        <Show when={done() && stored() !== false}>
          <span class="file-card__actions">
            <button
              class="btn btn--secondary"
              disabled={busy()}
              onClick={() => void act(() => api.openFile(props.file))}
            >
              <Icon name={api.kind === "web" ? "download" : "external-link"} size={18} />{" "}
              {api.kind === "web" ? t().file_save : t().file_open}
            </button>
            <Show when={api.capabilities.revealFile}>
              <button
                class="btn btn--ghost"
                disabled={busy()}
                onClick={() => void act(() => api.revealFile(props.file.file_id))}
              >
                <Icon name="folder-open" size={18} /> {t().file_reveal}
              </button>
            </Show>
          </span>
        </Show>
      </span>
    </div>
  );
}
