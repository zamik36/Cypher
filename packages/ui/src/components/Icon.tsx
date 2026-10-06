/*
 * Icons drawn after Lucide (https://lucide.dev, ISC licence): 24×24 grid,
 * 2 px round strokes. Decorative unless given a label.
 */
import { For, splitProps, type JSX } from "solid-js";

const PATHS = {
  "arrow-left": ["m12 19-7-7 7-7", "M19 12H5"],
  "chevron-right": ["m9 18 6-6-6-6"],
  plus: ["M5 12h14", "M12 5v14"],
  x: ["M18 6 6 18", "m6 6 12 12"],
  search: ["m21 21-4.3-4.3", "M11 19a8 8 0 1 0 0-16 8 8 0 0 0 0 16Z"],
  settings: [
    "M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z",
    "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z",
  ],
  send: ["M3.7 3.3 21 12 3.7 20.7 6.5 12Z", "M6.5 12H13"],
  paperclip: ["m21.4 11.1-9.2 9.2a6 6 0 0 1-8.5-8.5l8.6-8.6a4 4 0 0 1 5.7 5.7l-8.6 8.6a2 2 0 0 1-2.9-2.9l8-8"],
  mic: ["M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z", "M19 10v2a7 7 0 0 1-14 0v-2", "M12 19v3"],
  video: [
    "m16 13 5.2 3.5a.5.5 0 0 0 .8-.4V7.9a.5.5 0 0 0-.8-.4L16 11",
    "M4 6h10a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2Z",
  ],
  play: ["M7 4.5v15l12-7.5z"],
  pause: ["M7 4h3v16H7z", "M14 4h3v16h-3z"],
  check: ["M20 6 9 17l-5-5"],
  "check-check": ["M18 6 7 17l-5-5", "m22 10-7.5 7.5L13 16"],
  clock: ["M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20Z", "M12 6v6l4 2"],
  alert: ["M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20Z", "M12 8v4", "M12 16h.01"],
  file: ["M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z", "M14 2v4a2 2 0 0 0 2 2h4"],
  image: [
    "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2Z",
    "M9 11a2 2 0 1 0 0-4 2 2 0 0 0 0 4Z",
    "m21 15-3.1-3.1a2 2 0 0 0-2.8 0L6 21",
  ],
  download: ["M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4", "m7 10 5 5 5-5", "M12 15V3"],
  "folder-open": [
    "m6 14 1.5-2.9A2 2 0 0 1 9.2 10H20a2 2 0 0 1 1.9 2.5l-1.5 6a2 2 0 0 1-2 1.5H4a2 2 0 0 1-2-2V5c0-1.1.9-2 2-2h3.9a2 2 0 0 1 1.7.9l.8 1.2a2 2 0 0 0 1.7.9H18a2 2 0 0 1 2 2v2",
  ],
  "external-link": ["M15 3h6v6", "M10 14 21 3", "M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"],
  copy: [
    "M10 8h10a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H10a2 2 0 0 1-2-2V10a2 2 0 0 1 2-2Z",
    "M4 16a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2",
  ],
  share: [
    "M18 8a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z",
    "M6 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z",
    "M18 22a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z",
    "m8.6 13.5 6.8 4",
    "m15.4 6.5-6.8 4",
  ],
  qr: ["M3 3h6v6H3z", "M15 3h6v6h-6z", "M3 15h6v6H3z", "M15 15h2v2h-2z", "M19 15h2", "M15 19h2v2", "M19 19h2v2h-2z"],
  scan: [
    "M3 7V5a2 2 0 0 1 2-2h2",
    "M17 3h2a2 2 0 0 1 2 2v2",
    "M21 17v2a2 2 0 0 1-2 2h-2",
    "M7 21H5a2 2 0 0 1-2-2v-2",
    "M7 12h10",
  ],
  shield: [
    "M20 13c0 5-3.5 7.5-7.7 9a1 1 0 0 1-.6 0C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.2-2.7a1.2 1.2 0 0 1 1.5 0C14.5 3.8 17 5 19 5a1 1 0 0 1 1 1z",
    "m9 12 2 2 4-4",
  ],
  trash: ["M3 6h18", "M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6", "M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"],
  pencil: ["M21.2 6.8a2.8 2.8 0 0 0-4-4L3.8 16.2a2 2 0 0 0-.5.8l-1.3 4.4a.5.5 0 0 0 .6.6l4.4-1.3a2 2 0 0 0 .8-.5Z"],
  user: ["M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2", "M12 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8Z"],
  palette: [
    "M12 22a10 10 0 1 1 10-10c0 2.8-2.2 4-4 4h-2.5a1.5 1.5 0 0 0-1 2.6A1.5 1.5 0 0 1 12 22Z",
    "M13.5 6.5h.01",
    "M17.5 10.5h.01",
    "M8.5 7.5h.01",
    "M6.5 12.5h.01",
  ],
  bell: ["M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9", "M10.3 21a1.9 1.9 0 0 0 3.4 0"],
  lock: ["M5 11h14a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a2 2 0 0 1 2-2Z", "M7 11V7a5 5 0 0 1 10 0v4"],
  database: [
    "M12 8c5 0 9-1.3 9-3s-4-3-9-3-9 1.3-9 3 4 3 9 3Z",
    "M3 5v14c0 1.7 4 3 9 3s9-1.3 9-3V5",
    "M3 12c0 1.7 4 3 9 3s9-1.3 9-3",
  ],
  info: ["M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20Z", "M12 16v-4", "M12 8h.01"],
  "wifi-off": [
    "M12 20h.01",
    "M8.5 16.4a5 5 0 0 1 7 0",
    "M2 8.8a15 15 0 0 1 4.2-2.6",
    "M5 12.9a10 10 0 0 1 5.2-2.7",
    "M19 12.9a10 10 0 0 0-2.1-1.4",
    "M22 8.8A15 15 0 0 0 11.5 5",
    "m2 2 20 20",
  ],
  "message-circle": ["M7.9 20A9 9 0 1 0 4 16.1L2 22Z"],
  reply: ["m9 17-5-5 5-5", "M20 18v-2a4 4 0 0 0-4-4H4"],
  more: [
    "M12 13a1 1 0 1 0 0-2 1 1 0 0 0 0 2Z",
    "M19 13a1 1 0 1 0 0-2 1 1 0 0 0 0 2Z",
    "M5 13a1 1 0 1 0 0-2 1 1 0 0 0 0 2Z",
  ],
  retry: ["M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8", "M3 3v5h5"],
} as const satisfies Record<string, readonly string[]>;

export type IconName = keyof typeof PATHS;

type Props = Omit<JSX.SvgSVGAttributes<SVGSVGElement>, "name"> & {
  name: IconName;
  size?: number;
  /** Spoken name; without it the icon is hidden from assistive technology. */
  label?: string;
};

export default function Icon(props: Props) {
  const [own, rest] = splitProps(props, ["name", "size", "label"]);
  return (
    <svg
      viewBox="0 0 24 24"
      width={own.size ?? 20}
      height={own.size ?? 20}
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      role={own.label ? "img" : undefined}
      aria-label={own.label}
      aria-hidden={own.label ? undefined : "true"}
      {...rest}
    >
      <For each={PATHS[own.name]}>{(d) => <path d={d} />}</For>
    </svg>
  );
}
