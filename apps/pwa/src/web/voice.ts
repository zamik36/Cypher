/** Browser voice notes: MediaRecorder for the file, an analyser for the meter. */

const TYPES = ["audio/webm;codecs=opus", "audio/ogg;codecs=opus", "audio/mp4"];
/** One loudness sample per 20 ms, matching the native encoder's frames. */
const FRAME_MS = 20;

export interface VoiceClip {
  blob: Blob;
  mime: string;
  durationMs: number;
  /** RMS per 20 ms frame; the worker turns it into the shared waveform. */
  frames: Float32Array;
}

/** Same dBFS mapping as the native meter: -60 dB → 0, 0 dB → 1. */
function level(rms: number): number {
  return rms <= 0 ? 0 : Math.min(1, Math.max(0, (20 * Math.log10(rms) + 60) / 60));
}

export class WebVoiceRecorder {
  private readonly chunks: Blob[] = [];
  private readonly frames: number[] = [];
  private readonly startedAt = performance.now();
  private readonly timer: ReturnType<typeof setInterval>;

  private constructor(
    private readonly stream: MediaStream,
    private readonly recorder: MediaRecorder,
    private readonly audio: AudioContext,
    analyser: AnalyserNode,
    onLevel: (level: number) => void,
  ) {
    recorder.ondataavailable = (e) => e.data.size > 0 && this.chunks.push(e.data);
    const buf = new Float32Array(analyser.fftSize);
    this.timer = setInterval(() => {
      analyser.getFloatTimeDomainData(buf);
      let sum = 0;
      for (const s of buf) sum += s * s;
      const rms = Math.sqrt(sum / buf.length);
      this.frames.push(rms);
      if (this.frames.length % 2 === 0) onLevel(level(rms));
    }, FRAME_MS);
    recorder.start(1000);
  }

  static async start(onLevel: (level: number) => void): Promise<WebVoiceRecorder> {
    const mimeType = TYPES.find((t) => MediaRecorder.isTypeSupported(t));
    if (!mimeType) throw new Error("voice recording is not supported here");
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
    });
    const audio = new AudioContext();
    const analyser = audio.createAnalyser();
    analyser.fftSize = 1024;
    audio.createMediaStreamSource(stream).connect(analyser);
    const recorder = new MediaRecorder(stream, { mimeType, audioBitsPerSecond: 32_000 });
    return new WebVoiceRecorder(stream, recorder, audio, analyser, onLevel);
  }

  async stop(): Promise<VoiceClip> {
    const durationMs = Math.round(performance.now() - this.startedAt);
    const stopped = new Promise((resolve) => (this.recorder.onstop = resolve));
    this.recorder.stop();
    await stopped;
    this.release();
    const mime = this.recorder.mimeType.split(";")[0] || "audio/webm";
    return {
      blob: new Blob(this.chunks, { type: mime }),
      mime,
      durationMs,
      frames: Float32Array.from(this.frames),
    };
  }

  cancel() {
    if (this.recorder.state === "recording") this.recorder.stop();
    this.release();
  }

  private release() {
    clearInterval(this.timer);
    for (const track of this.stream.getTracks()) track.stop();
    void this.audio.close();
  }
}
