import { t } from "../i18n";

/**
 * A failure in the user's language. Both platforms report the core's
 * `FailReason` by its snake_case name (e.g. `key_mismatch`); anything else
 * is shown as is.
 */
export function reasonText(error: unknown): string {
  const raw = rawReason(error);
  const tr = t();
  const known: Record<string, string> = {
    invalid_link: tr.join_invalid_link,
    not_found: tr.join_not_found,
    self_link: tr.join_self_link,
    key_mismatch: tr.join_key_mismatch,
    invalid_keys: tr.join_invalid_keys,
    timeout: tr.join_timeout,
    offline: tr.join_offline,
    update_required: tr.error_update_required,
    storage_failed: tr.error_storage_failed,
    corrupted: tr.error_corrupted,
    rejected: tr.error_rejected,
    // The core's, and waiting to be linked that stopped (both platforms).
    cancelled: tr.error_cancelled,
    source_unavailable: tr.error_source_unavailable,
    unauthorized: tr.error_unauthorized,
    server_error: tr.error_server,
    decrypt_failed: tr.error_decrypt_failed,
    // The desktop's client is still starting, or missed a reply.
    connecting: tr.error_connecting,
    lagged: tr.error_lagged,
    // Opening a received file.
    unsafe_type: tr.file_unsafe,
    no_app: tr.file_no_app,
    not_saved: tr.file_missing,
    // Identity file and recovery phrase, as both platforms word them.
    "wrong passphrase": tr.identity_err_wrong_passphrase,
    "passphrase is too short": tr.identity_err_short,
    "identity file is corrupt": tr.identity_err_corrupt,
    "identity data is corrupt": tr.identity_err_corrupt,
    "invalid recovery phrase": tr.identity_err_bad_phrase,
    "an identity already exists": tr.identity_err_exists,
    "nickname is too long": tr.identity_err_nickname,
  };
  return known[raw] ?? raw;
}

/** The failure as the platform reported it. */
function rawReason(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** Whether a command failed only because the client is still starting. */
export function isConnecting(error: unknown): boolean {
  return rawReason(error) === "connecting";
}
