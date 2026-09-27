import type { VideoNote } from "../platform";

/** Round video notes are square, like Telegram's, and capped at a minute. */
export const VIDEO_NOTE_SIZE = 384;
export const MAX_VIDEO_NOTE_MS = 60_000;
const FPS = 30;
const POSTER_SIZE = 160;
/** Must fit the envelope's poster limit. */
const MAX_POSTER_BYTES = 16 * 1024;
const TYPES = [
  "video/webm;codecs=vp9,opus",
  "video/webm;codecs=vp8,opus",
  "video/webm",
  "video/mp4;codecs=avc1,mp4a",
  "video/mp4",
];

function pickType(): string {
  const type = TYPES.find((t) => MediaRecorder.isTypeSupported(t));
  if (!type) throw new Error("video recording is not supported here");
  return type;
}

/**
 * Records the front camera, centre-cropped to a square on a canvas so every
 * platform produces the same compact format. `preview` shows the live crop.
 */
export class VideoNoteRecorder {
  private readonly chunks: Blob[] = [];
  private readonly canvas = document.createElement("canvas");
  private readonly camera = document.createElement("video");
  private recorder: MediaRecorder | null = null;
  private frame = 0;
  private startedAt = 0;
  private limit: ReturnType<typeof setTimeout> | undefined;
  private poster: Promise<Uint8Array> | null = null;

  private constructor(private readonly stream: MediaStream) {
    this.canvas.width = this.canvas.height = VIDEO_NOTE_SIZE;
    this.camera.srcObject = stream;
    this.camera.muted = true;
    this.camera.playsInline = true;
  }

  static async open(): Promise<VideoNoteRecorder> {
    const stream = await navigator.mediaDevices.getUserMedia({
      video: { facingMode: "user", width: { ideal: 640 }, height: { ideal: 640 } },
      audio: { echoCancellation: true, noiseSuppression: true },
    });
    const rec = new VideoNoteRecorder(stream);
    await rec.camera.play();
    return rec;
  }

  /** The cropped live stream, for a mirrored round preview. */
  get preview(): MediaStream {
    return this.canvas.captureStream(FPS);
  }

  /** Starts recording; `onLimit` fires when the length cap stops it. */
  start(onLimit: () => void) {
    const type = pickType();
    this.draw();
    const stream = new MediaStream([
      ...this.canvas.captureStream(FPS).getVideoTracks(),
      ...this.stream.getAudioTracks(),
    ]);
    const recorder = new MediaRecorder(stream, {
      mimeType: type,
      videoBitsPerSecond: 600_000,
      audioBitsPerSecond: 48_000,
    });
    recorder.ondataavailable = (e) => e.data.size > 0 && this.chunks.push(e.data);
    recorder.start(1000);
    this.recorder = recorder;
    this.startedAt = performance.now();
    this.limit = setTimeout(onLimit, MAX_VIDEO_NOTE_MS);
    this.poster = new Promise((resolve) => setTimeout(() => resolve(this.capturePoster()), 300));
  }

  elapsedMs(): number {
    return this.startedAt ? performance.now() - this.startedAt : 0;
  }

  async stop(): Promise<VideoNote> {
    const recorder = this.recorder;
    if (!recorder) throw new Error("not recording");
    const durationMs = Math.round(Math.min(this.elapsedMs(), MAX_VIDEO_NOTE_MS));
    const stopped = new Promise((resolve) => (recorder.onstop = resolve));
    recorder.stop();
    await stopped;
    const poster = await (this.poster ?? this.capturePoster());
    this.release();
    const mime = recorder.mimeType.split(";")[0] || "video/webm";
    return { blob: new Blob(this.chunks, { type: mime }), mime, durationMs, poster };
  }

  cancel() {
    if (this.recorder?.state === "recording") this.recorder.stop();
    this.release();
  }

  private draw = () => {
    const ctx = this.canvas.getContext("2d");
    const { videoWidth: w, videoHeight: h } = this.camera;
    if (ctx && w && h) {
      const side = Math.min(w, h);
      ctx.drawImage(this.camera, (w - side) / 2, (h - side) / 2, side, side, 0, 0, VIDEO_NOTE_SIZE, VIDEO_NOTE_SIZE);
    }
    this.frame = requestAnimationFrame(this.draw);
  };

  private async capturePoster(): Promise<Uint8Array> {
    const small = document.createElement("canvas");
    small.width = small.height = POSTER_SIZE;
    small.getContext("2d")?.drawImage(this.canvas, 0, 0, POSTER_SIZE, POSTER_SIZE);
    for (const quality of [0.7, 0.5, 0.3]) {
      const blob = await new Promise<Blob | null>((r) => small.toBlob(r, "image/jpeg", quality));
      if (blob && blob.size <= MAX_POSTER_BYTES) return new Uint8Array(await blob.arrayBuffer());
    }
    return new Uint8Array();
  }

  private release() {
    clearTimeout(this.limit);
    cancelAnimationFrame(this.frame);
    for (const track of this.stream.getTracks()) track.stop();
  }
}
