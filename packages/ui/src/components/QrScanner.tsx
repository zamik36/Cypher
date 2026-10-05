import { createSignal, onCleanup, onMount, Show } from "solid-js";
import "./QrScanner.css";
import Icon from "./Icon";
import { api } from "../platform";
import { findInvite } from "../utils/invite";
import { reasonText } from "../utils/reasons";
import { t } from "../i18n";

/** Chromium's shape detection; not in every browser nor in TypeScript's DOM types. */
interface Detector {
  detect(source: CanvasImageSource): Promise<{ rawValue: string }[]>;
}
interface DetectorClass {
  new (options: { formats: string[] }): Detector;
  getSupportedFormats(): Promise<string[]>;
}

const FRAME_MS = 200;
/** Frames are scaled down to this width before decoding: plenty for a QR. */
const DECODE_WIDTH = 640;

type Decode = (video: HTMLVideoElement) => Promise<string | null>;

/** The fastest decoder available: the browser's own, else jsQR (loaded on first use). */
async function decoder(): Promise<Decode> {
  const Native = (globalThis as { BarcodeDetector?: DetectorClass }).BarcodeDetector;
  if (Native && (await Native.getSupportedFormats()).includes("qr_code")) {
    const detector = new Native({ formats: ["qr_code"] });
    return async (video) => (await detector.detect(video))[0]?.rawValue ?? null;
  }
  const { default: jsQR } = await import("jsqr");
  const canvas = document.createElement("canvas");
  const context = canvas.getContext("2d", { willReadFrequently: true });
  return (video) => {
    if (!context || video.videoWidth === 0) return Promise.resolve(null);
    const scale = Math.min(1, DECODE_WIDTH / video.videoWidth);
    canvas.width = Math.round(video.videoWidth * scale);
    canvas.height = Math.round(video.videoHeight * scale);
    context.drawImage(video, 0, 0, canvas.width, canvas.height);
    const { data, width, height } = context.getImageData(0, 0, canvas.width, canvas.height);
    return Promise.resolve(jsQR(data, width, height, { inversionAttempts: "dontInvert" })?.data ?? null);
  };
}

function cameraError(e: unknown): string {
  const name = e instanceof DOMException ? e.name : "";
  if (name === "NotAllowedError" || name === "SecurityError") return t().scan_denied;
  if (name === "NotFoundError" || name === "OverconstrainedError") return t().scan_no_camera;
  return reasonText(e);
}

/**
 * Reads an invite QR with the camera and hands back its code. On Android
 * the system scanner runs behind a transparent page; elsewhere the camera
 * shows in a frame here.
 */
export default function QrScanner(props: { onCode: (code: string) => void; onClose: () => void }) {
  const [error, setError] = createSignal<string | null>(null);
  const native = api.scanQr;
  let video: HTMLVideoElement | undefined;
  let stream: MediaStream | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  // Read from callbacks that outlive the component.
  const run = { stopped: false };

  /** A code read from a QR: an invite goes back, anything else is said so. */
  function read(text: string): boolean {
    const code = findInvite(text);
    if (code) props.onCode(code);
    else setError(t().scan_not_invite);
    return code !== null;
  }

  async function scanNatively(scan: () => Promise<string>) {
    document.documentElement.classList.add("native-scan");
    try {
      while (!run.stopped && !read(await scan()));
    } catch (e) {
      if (!run.stopped) setError(cameraError(e));
    } finally {
      document.documentElement.classList.remove("native-scan");
    }
  }

  async function scanWithCamera() {
    try {
      stream = await navigator.mediaDevices.getUserMedia({
        video: { facingMode: "environment", width: { ideal: 1280 } },
        audio: false,
      });
      const view = video;
      if (run.stopped || !view) {
        // Closed while the camera was starting.
        stream.getTracks().forEach((track) => track.stop());
        return;
      }
      view.srcObject = stream;
      await view.play();
      const decode = await decoder();
      const tick = async () => {
        if (run.stopped) return;
        const text = await decode(view).catch(() => null);
        if (text && read(text)) return;
        timer = setTimeout(() => void tick(), FRAME_MS);
      };
      void tick();
    } catch (e) {
      if (!run.stopped) setError(cameraError(e));
    }
  }

  onMount(() => void (native ? scanNatively(native) : scanWithCamera()));
  onCleanup(() => {
    run.stopped = true;
    clearTimeout(timer);
    stream?.getTracks().forEach((track) => track.stop());
    if (native) void api.cancelScan?.().catch(() => undefined);
    document.documentElement.classList.remove("native-scan");
  });

  return (
    <div
      class="qr-scanner"
      classList={{ "qr-scanner--native": Boolean(native) }}
      role="dialog"
      aria-label={t().scan_button}
    >
      <Show when={!native}>
        <video ref={video} class="qr-scanner__video" muted playsinline />
      </Show>
      <div class="qr-scanner__frame" aria-hidden="true" />
      <div class="qr-scanner__bottom">
        <p class="qr-scanner__hint" role={error() ? "alert" : undefined}>
          {error() ?? t().scan_hint}
        </p>
        <button class="btn btn--secondary" onClick={() => props.onClose()}>
          <Icon name="x" size={18} /> {t().common_cancel}
        </button>
      </div>
    </div>
  );
}
