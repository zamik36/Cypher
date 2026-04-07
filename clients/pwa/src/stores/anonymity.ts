import { createStore } from "solid-js/store";

export interface AnonymousSettings {
  enabled: boolean;
  bridgeLines: string[];
}

export interface AnonymityStatus {
  supported: boolean;
  label: string;
  description: string;
}

const SETTINGS_KEY = "cypher-anonymous-settings";

function loadSettings(): AnonymousSettings {
  const fallback: AnonymousSettings = { enabled: true, bridgeLines: [] };
  const raw = localStorage.getItem(SETTINGS_KEY);
  if (!raw) return fallback;

  try {
    const parsed = JSON.parse(raw) as Partial<AnonymousSettings>;
    return {
      enabled: parsed.enabled ?? true,
      bridgeLines: Array.isArray(parsed.bridgeLines)
        ? parsed.bridgeLines.filter((line): line is string => typeof line === "string")
        : [],
    };
  } catch {
    return fallback;
  }
}

const [anonymousSettings, setAnonymousSettingsStore] = createStore<AnonymousSettings>(loadSettings());
const [anonymityStatus, setAnonymityStatus] = createStore<AnonymityStatus>({
  supported: false,
  label: "Web build",
  description: "Bridge settings are stored locally, but Tor transport is not active in the PWA build.",
});

export function setAnonymousSettings(settings: AnonymousSettings) {
  setAnonymousSettingsStore(settings);
  localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
}

export { anonymousSettings, anonymityStatus, setAnonymityStatus };
