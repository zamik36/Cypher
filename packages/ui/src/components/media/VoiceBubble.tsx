import { createSignal, For, onCleanup, Show } from "solid-js";
import { api, type UiFile } from "../../platform";
import { mediaProgress, mediaReady } from "../../stores/media";
import { addToast, toastError } from "../../stores/toasts";
import { t } from "../../i18n";
import { claimPlayback, formatDuration } from "./player";

const SPEEDS = [1, 1.5, 2];

export default function VoiceBubble(props: { file: UiFile }) {
  const [position, setPosition] = createSignal(0);
  const [playing, setPlaying] = createSignal(false);
  const [speed, setSpeed] = createSignal(1);
  let audio: HTMLAudioElement | undefined;

  const durationMs = () => props.file.duration_ms ?? 0;
  const bars = () => {
    const w = props.file.waveform ?? [];
    return w.length > 0 ? w : new Array<number>(64).fill(0);
  };

  async function element(): Promise<HTMLAudioElement> {
    if (audio) return audio;
    const el = new Audio(await api.mediaUrl(props.file.file_id));
    el.playbackRate = speed();
    // MediaRecorder files often lack a duration; the message carries it.
    el.ontimeupdate = () => setPosition(Math.min(1, (el.currentTime * 1000) / (durationMs() || 1)));
    el.onplay = () => setPlaying(true);
    el.onpause = () => setPlaying(false);
    el.onended = () => {
      setPlaying(false);
      setPosition(0);
    };
    audio = el;
    return el;
  }

  async function toggle() {
    if (!mediaReady(props.file.file_id)) {
      addToast(t().media_downloading, "info");
      return;
    }
    try {
      const el = await element();
      if (el.paused) {
        claimPlayback(el);
        await el.play();
      } else {
        el.pause();
      }
    } catch (e) {
      toastError(e);
    }
  }

  async function seek(e: MouseEvent) {
    const ratio = e.offsetX / (e.currentTarget as HTMLElement).clientWidth;
    const el = await element().catch(() => undefined);
    if (!el) return;
    el.currentTime = (ratio * durationMs()) / 1000;
    setPosition(ratio);
  }

  function cycleSpeed() {
    const next = SPEEDS[(SPEEDS.indexOf(speed()) + 1) % SPEEDS.length] ?? 1;
    setSpeed(next);
    if (audio) audio.playbackRate = next;
  }

  onCleanup(() => audio?.pause());

  return (
    <div class="voice-bubble">
      <button class="voice-play" onClick={toggle} aria-label={playing() ? "pause" : "play"}>
        <Show
          when={mediaReady(props.file.file_id)}
          fallback={<span class="voice-loading">{Math.round(mediaProgress(props.file.file_id) * 100)}%</span>}
        >
          <Show
            when={playing()}
            fallback={
              <svg viewBox="0 0 24 24" width="18" height="18">
                <path fill="currentColor" d="M8 5v14l11-7z" />
              </svg>
            }
          >
            <svg viewBox="0 0 24 24" width="18" height="18">
              <path fill="currentColor" d="M6 5h4v14H6zm8 0h4v14h-4z" />
            </svg>
          </Show>
        </Show>
      </button>
      <div class="voice-body">
        <div class="voice-wave" onClick={seek}>
          <For each={bars()}>
            {(v, i) => (
              <span
                class={`voice-bar ${i() / bars().length < position() ? "played" : ""}`}
                style={{ height: `${Math.max(12, (v / 255) * 100)}%` }}
              />
            )}
          </For>
        </div>
        <div class="voice-meta">
          <span>{formatDuration(playing() || position() > 0 ? position() * durationMs() : durationMs())}</span>
          <button class="voice-speed" onClick={cycleSpeed}>
            {speed()}×
          </button>
        </div>
      </div>
    </div>
  );
}
