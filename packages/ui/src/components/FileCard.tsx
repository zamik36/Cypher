import { createResource, createSignal, Match, Show, Switch } from "solid-js";
import ActionMenu, { type MenuAction } from "./ActionMenu";
import Icon, { type IconName } from "./Icon";
import ImageViewer from "./ImageViewer";
import { api, type TransferInfo, type UiFile } from "../platform";
import { transferOf, upsertTransfer } from "../stores/transfers";
import { toastError } from "../stores/toasts";
import { formatBytes } from "../utils/format";
import { locale, t } from "../i18n";

const IMAGE = /\.(avif|bmp|gif|heic|jpe?g|png|svg|webp)$/i;

type State = "offered" | "active" | "error" | "done" | "idle";

/**
 * A file in the conversation: what it is and how far it got. Tapping it
 * opens a menu with what can be done now: download or decline an offer,
 * cancel a transfer, open a finished file or show it in its folder.
 */
export default function FileCard(props: { file: UiFile; outgoing: boolean }) {
  const [menu, setMenu] = createSignal(false);
  const [viewing, setViewing] = createSignal(false);
  const [broken, setBroken] = createSignal(false);
  let card: HTMLButtonElement | undefined;
  const id = () => props.file.file_id;
  const transfer = () => transferOf(id());
  const percent = () => Math.round((transfer()?.progress ?? 0) * 100);
  // A file finished in an earlier session has no transfer here: ask whether
  // it is still on this device. Asked again when a transfer completes.
  const [stored] = createResource(
    () => (transfer()?.status ?? "unknown") as string,
    (status) => (status === "unknown" || status === "complete" ? api.fileSaved(id()) : false),
  );

  const state = (): State => {
    const status = transfer()?.status;
    if (status === "offered" && !props.outgoing) return "offered";
    if (status === "active") return "active";
    if (status === "error") return "error";
    if (status === "complete" || (!transfer() && stored() === true)) return "done";
    return "idle";
  };

  const image = () => props.file.mime.startsWith("image/") || IMAGE.test(props.file.name);
  // A kept picture shows itself instead of an icon.
  const [preview] = createResource(
    () => image() && state() === "done" && stored() !== false && id(),
    (fileId) => api.imageUrl(fileId).catch(() => undefined),
  );
  const shown = () => (broken() ? undefined : preview());

  async function act(action: () => Promise<void>, after?: Partial<TransferInfo>) {
    try {
      await action();
      if (after) upsertTransfer({ ...after, file_id: id() });
    } catch (e) {
      toastError(e);
    }
  }

  // Each reads the file once, when chosen.
  function download() {
    const fileId = id();
    void act(() => api.acceptFile(fileId), { status: "active" });
  }
  function decline() {
    const [fileId, error] = [id(), t().file_declined];
    void act(() => api.cancelTransfer(fileId), { status: "error", error });
  }
  function cancel() {
    const [fileId, error] = [id(), t().error_cancelled];
    void act(() => api.cancelTransfer(fileId), { status: "error", error });
  }
  function open() {
    const file = props.file;
    void act(() => api.openFile(file));
  }
  function reveal() {
    const fileId = id();
    void act(() => api.revealFile(fileId));
  }

  const actions = (): MenuAction[] => {
    const tr = t();
    switch (state()) {
      case "offered":
        return [
          { label: tr.file_accept, icon: "download", run: download },
          { label: tr.file_decline, icon: "x", danger: true, run: decline },
        ];
      case "active":
        return [{ label: tr.file_cancel, icon: "x", danger: true, run: cancel }];
      case "done": {
        if (stored() === false) return [];
        const view: MenuAction[] = shown() ? [{ label: tr.file_view, icon: "image", run: () => setViewing(true) }] : [];
        const first: MenuAction =
          api.kind === "web"
            ? { label: tr.file_save, icon: "download", run: open }
            : { label: tr.file_open, icon: "external-link", run: open };
        const folder: MenuAction = { label: tr.file_reveal, icon: "folder-open", run: reveal };
        return [...view, first, ...(api.capabilities.revealFile ? [folder] : [])];
      }
      default:
        return [];
    }
  };

  const icon = (): IconName => {
    if (state() === "offered") return "download";
    if (state() === "error") return "alert";
    return image() ? "image" : "file";
  };

  const size = () => formatBytes(props.file.size, locale());

  return (
    <>
      <button
        ref={card}
        class="file-card"
        data-testid="file-card"
        data-state={state() === "done" ? "complete" : (transfer()?.status ?? "idle")}
        disabled={actions().length === 0}
        aria-haspopup="menu"
        aria-expanded={menu()}
        aria-label={props.file.name}
        onClick={() => setMenu(!menu())}
        classList={{ "file-card--image": Boolean(shown()) }}
      >
        <Show
          when={shown()}
          fallback={
            <span class="file-card__icon" data-state={state()}>
              <Icon name={icon()} />
            </span>
          }
        >
          {(src) => <img class="file-card__image" src={src()} alt="" onError={() => setBroken(true)} />}
        </Show>
        <span class="file-card__body">
          <span class="file-card__name" title={props.file.name}>
            {props.file.name}
          </span>
          <span class="file-card__meta">
            <Switch fallback={size()}>
              <Match when={state() === "offered"}>
                {size()} · {t().file_tap_to_download}
              </Match>
              <Match when={state() === "active"}>
                {size()} · {percent()}%
              </Match>
              <Match when={state() === "error"}>
                <span class="error-text">{transfer()?.error ?? t().file_failed}</span>
              </Match>
              <Match when={state() === "done"}>
                {size()} · {props.outgoing ? t().file_sent : t().file_saved}
              </Match>
            </Switch>
          </span>
          <Show when={state() === "active"}>
            <span class="file-card__progress" aria-hidden="true">
              <span style={{ width: `${percent()}%` }} />
            </span>
          </Show>
        </span>
      </button>
      <Show when={viewing() && shown()}>
        {(src) => <ImageViewer src={src()} alt={props.file.name} onClose={() => setViewing(false)} />}
      </Show>
      <Show when={menu() && card && actions().length > 0}>
        <ActionMenu
          anchor={card as HTMLElement}
          align={props.outgoing ? "end" : "start"}
          actions={actions()}
          label={t().file_actions}
          onClose={() => setMenu(false)}
        />
      </Show>
    </>
  );
}
