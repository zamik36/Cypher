import { t } from "../i18n";

/**
 * Why joining a room failed, in the user's language. Both platforms report
 * the core's `FailReason` name (e.g. `KeyMismatch`); anything else is shown
 * as is.
 */
export function joinErrorText(error: unknown): string {
  const raw = error instanceof Error ? error.message : String(error);
  const tr = t();
  const known: Record<string, string> = {
    InvalidLink: tr.join_invalid_link,
    NotFound: tr.join_not_found,
    SelfLink: tr.join_self_link,
    KeyMismatch: tr.join_key_mismatch,
    InvalidKeys: tr.join_invalid_keys,
    Timeout: tr.join_timeout,
    Offline: tr.join_offline,
  };
  return known[raw] ?? raw;
}
