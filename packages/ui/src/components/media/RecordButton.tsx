import { createSignal, onCleanup, Show } from "solid-js";
import { api, type MediaSent, type UiFile } from "../../platform";
import { MAX_VIDEO_NOTE_MS, VideoNoteRecorder } from "../../media/videoNote";
import { addToast, toastError } from "../../stores/toasts";
import { t } from "../../i18n";
import { formatDuration } from "./player";
import Icon from "../Icon";

type Mode = "voice" | "video";
type Phase = "idle" | "starting" | "recording";

/** A press shorter than this switches mode instead of recording. */
const HOLD_MS = 200;
/** Dragging this far left while recording cancels it. */
const CANCEL_PX = 90;

interface Props {
  peer: string;
  onSent: (sent: MediaSent, file: UiFile) => void;
}

/**
 * Hold to record a voice or round video note, release to send, slide left
 * to cancel; a quick tap toggles between voice and video.
 */
export default function RecordButton(props: Props) {
  const [mode, setMode] = createSignal<Mode>("voice");
  const [phase, setPhase] = createSignal<Phase>("idle");
  const [elapsed, setElapsed] = createSignal(0);
  const [level, setLevel] = createSignal(0);
  const [dx, setDx] = createSignal(0);
  let holdTimer: ReturnType<typeof setTimeout> | undefined;
  let clock: ReturnType<typeof setInterval> | undefined;
  let startX = 0;
  let startedAt = 0;
  let video: VideoNoteRecorder | null = null;
  let preview: HTMLVideoElement | undefined;
  let finishing = false;

  const cancelling = () => dx() < -CANCEL_PX;

  async function begin() {
    setPhase("starting");
    try {
      if (mode() === "voice") {
        await api.startVoice(setLevel);
      } else {
        video = await VideoNoteRecorder.open();
        if (preview) preview.srcObject = video.preview;
        video.start(() => void finish(false));
      }
      if (phase() !== "starting") {
        await discard();
        return;
      }
      startedAt = performance.now();
      clock = setInterval(() => setElapsed(performance.now() - startedAt), 100);
      setPhase("recording");
    } catch (e) {
      setPhase("idle");
      toastError(e);
    }
  }

  async function discard() {
    if (video) video.cancel();
    else await api.cancelVoice().catch(() => undefined);
    video = null;
  }

  async function finish(cancel: boolean) {
    if (finishing) return;
    finishing = true;
    clearInterval(clock);
    const wasVideo = video !== null;
    setPhase("idle");
    setElapsed(0);
    setDx(0);
    setLevel(0);
    try {
      if (cancel) {
        await discard();
      } else if (wasVideo && video) {
        const note = await video.stop();
        video = null;
        const sent = await api.sendVideoNote(props.peer, note);
        props.onSent(sent, {
          file_id: sent.file_id,
          name: "",
          size: note.blob.size,
          mime: note.mime,
          kind: "video_note",
          duration_ms: sent.duration_ms,
          poster: Array.from(note.poster),
        });
      } else {
        const sent = await api.stopVoice(props.peer);
        if (!sent) {
          addToast(t().media_too_short, "info");
        } else {
          props.onSent(sent, {
            file_id: sent.file_id,
            name: "",
            size: 0,
            mime: "audio/webm",
            kind: "voice",
            duration_ms: sent.duration_ms,
            waveform: sent.waveform ?? null,
          });
        }
      }
    } catch (e) {
      toastError(e);
    } finally {
      finishing = false;
    }
  }

  function onDown(e: PointerEvent) {
    if (phase() !== "idle") return;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    startX = e.clientX;
    holdTimer = setTimeout(() => {
      holdTimer = undefined;
      void begin();
    }, HOLD_MS);
  }

  function onMove(e: PointerEvent) {
    if (phase() !== "idle") setDx(Math.min(0, e.clientX - startX));
  }

  function onUp() {
    if (holdTimer) {
      clearTimeout(holdTimer);
      holdTimer = undefined;
      setMode((m) => (m === "voice" ? "video" : "voice"));
      return;
    }
    if (phase() === "starting") setPhase("idle");
    else if (phase() === "recording") void finish(cancelling());
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape" && phase() === "recording") void finish(true);
  }
  window.addEventListener("keydown", onKey);
  onCleanup(() => {
    window.removeEventListener("keydown", onKey);
    if (phase() !== "idle") void finish(true);
  });

  return (
    <>
      <Show when={phase() === "recording" && mode() === "voice"}>
        <div
          class={`recording-bar ${cancelling() ? "cancelling" : ""}`}
          style={{ transform: `translateX(${dx() / 3}px)` }}
        >
          <span class="rec-dot" style={{ transform: `scale(${1 + level() * 0.8})` }} />
          <span class="rec-time">{formatDuration(elapsed())}</span>
          <span class="rec-hint">{cancelling() ? t().media_release_cancel : t().media_slide_cancel}</span>
        </div>
      </Show>
      <Show when={phase() !== "idle" && mode() === "video"}>
        <div class={`video-note-overlay ${cancelling() ? "cancelling" : ""}`}>
          <div class="video-note-preview">
            <video ref={preview} autoplay muted playsinline />
            <svg class="round-ring recording" viewBox="0 0 100 100">
              <circle
                cx="50"
                cy="50"
                r="48"
                stroke-dasharray={`${2 * Math.PI * 48}`}
                stroke-dashoffset={`${2 * Math.PI * 48 * (1 - elapsed() / MAX_VIDEO_NOTE_MS)}`}
              />
            </svg>
          </div>
          <span class="rec-time">{formatDuration(elapsed())}</span>
          <span class="rec-hint">{cancelling() ? t().media_release_cancel : t().media_slide_cancel}</span>
        </div>
      </Show>
      <button
        class={`icon-btn record-btn ${phase() !== "idle" ? "active" : ""}`}
        aria-label={mode() === "voice" ? t().media_hold_voice : t().media_hold_video}
        title={mode() === "voice" ? t().media_hold_voice : t().media_hold_video}
        onPointerDown={onDown}
        onPointerMove={onMove}
        onPointerUp={onUp}
        onPointerCancel={() => phase() === "recording" && void finish(true)}
        onContextMenu={(e) => e.preventDefault()}
      >
        <Icon name={mode() === "voice" ? "mic" : "video"} />
      </button>
    </>
  );
}
