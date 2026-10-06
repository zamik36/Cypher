import { createSignal, For, onCleanup, Show, type JSX } from "solid-js";
import Icon from "../Icon";
import { api, type UiFile } from "../../platform";
import { mediaPlayable, mediaProgress } from "../../stores/media";
import { addToast, toastError } from "../../stores/toasts";
import { t } from "../../i18n";
import { claimPlayback, formatDuration } from "./player";

const SPEEDS = [1, 1.5, 2];

/** A voice note; `meta` (time and ticks) sits at the end of its bottom row. */
export default function VoiceBubble(props: { file: UiFile; meta?: JSX.Element }) {
  const [position, setPosition] = createSignal(0);
  const [playing, setPlaying] = createSignal(false);
  const [speed, setSpeed] = createSignal(1);
  let audio: HTMLAudioElement | undefined;

  const durationMs = () => props.file.duration_ms ?? 0;
  const bars = () => {
    const w = props.file.waveform ?? [];
    return w.length > 0 ? w : new Array<number>(64).fill(0);
  };

  const playable = () => mediaPlayable(props.file.file_id, api.capabilities.streamMedia);

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
    if (!playable()) {
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
      <button class="voice-play" onClick={toggle} aria-label={playing() ? t().media_pause : t().media_play}>
        <Show
          when={playable()}
          fallback={<span class="voice-loading">{Math.round(mediaProgress(props.file.file_id) * 100)}%</span>}
        >
          <Icon name={playing() ? "pause" : "play"} size={18} fill="currentColor" stroke="none" />
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
          <span class="voice-time">
            {formatDuration(playing() || position() > 0 ? position() * durationMs() : durationMs())}
          </span>
          <button class="voice-speed" onClick={cycleSpeed} aria-label={t().media_speed(speed())}>
            {speed()}×
          </button>
          {props.meta}
        </div>
      </div>
    </div>
  );
}
