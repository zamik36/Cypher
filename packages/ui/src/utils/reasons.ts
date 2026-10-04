import { t } from "../i18n";

/**
 * A failure in the user's language. Both platforms report the core's
 * `FailReason` name (e.g. `KeyMismatch`); anything else is shown as is.
 */
export function reasonText(error: unknown): string {
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
    UpdateRequired: tr.error_update_required,
    StorageFailed: tr.error_storage_failed,
    Corrupted: tr.error_corrupted,
    Rejected: tr.error_rejected,
    Cancelled: tr.error_cancelled,
    SourceUnavailable: tr.error_source_unavailable,
    Unauthorized: tr.error_unauthorized,
    ServerError: tr.error_server,
    DecryptFailed: tr.error_decrypt_failed,
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
