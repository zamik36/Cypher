import { createMemo, createSignal, onCleanup, Show } from "solid-js";
import { api, type UiFile } from "../../platform";
import { mediaProgress, mediaReady } from "../../stores/media";
import { addToast } from "../../stores/toasts";
import { t } from "../../i18n";
import { claimPlayback, formatDuration } from "./player";

const RADIUS = 48;
const CIRCUMFERENCE = 2 * Math.PI * RADIUS;

/** Telegram-style round video: tap to play or pause, ring shows progress. */
export default function RoundVideoBubble(props: { file: UiFile }) {
  const [src, setSrc] = createSignal<string>();
  const [playing, setPlaying] = createSignal(false);
  const [position, setPosition] = createSignal(0);
  let video: HTMLVideoElement | undefined;

  const poster = createMemo(() => {
    const bytes = props.file.poster;
    return bytes && bytes.length > 0
      ? URL.createObjectURL(new Blob([new Uint8Array(bytes)], { type: "image/jpeg" }))
      : undefined;
  });
  onCleanup(() => {
    const url = poster();
    if (url) URL.revokeObjectURL(url);
    video?.pause();
  });

  const durationMs = () => props.file.duration_ms ?? 0;
  const ring = () => (mediaReady(props.file.file_id) ? position() : mediaProgress(props.file.file_id));

  async function toggle() {
    if (!mediaReady(props.file.file_id)) {
      addToast(t().media_downloading, "info");
      return;
    }
    try {
      if (!src()) setSrc(await api.mediaUrl(props.file.file_id));
      if (!video) return;
      if (video.paused) {
        claimPlayback(video);
        await video.play();
      } else {
        video.pause();
      }
    } catch (e) {
      addToast(String(e), "error");
    }
  }

  return (
    <button class={`round-video ${playing() ? "playing" : ""}`} onClick={toggle}>
      <video
        ref={video}
        src={src()}
        poster={poster()}
        playsinline
        preload="metadata"
        onPlay={() => setPlaying(true)}
        onPause={() => setPlaying(false)}
        onEnded={() => {
          setPlaying(false);
          setPosition(0);
        }}
        onTimeUpdate={(e) => setPosition(Math.min(1, (e.currentTarget.currentTime * 1000) / (durationMs() || 1)))}
      />
      <svg class="round-ring" viewBox="0 0 100 100">
        <circle
          cx="50"
          cy="50"
          r={RADIUS}
          stroke-dasharray={`${CIRCUMFERENCE}`}
          stroke-dashoffset={`${CIRCUMFERENCE * (1 - ring())}`}
        />
      </svg>
      <Show when={!playing()}>
        <span class="round-duration">{formatDuration(durationMs())}</span>
      </Show>
    </button>
  );
}
