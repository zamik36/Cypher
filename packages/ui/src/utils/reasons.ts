import type { ErrorKind, FailReason, IdentityKind } from "../errors";
import { isCmdErrorShape } from "../errors";
import { t } from "../i18n";

type Words = ReturnType<typeof t>;

/** Every kind of failure in words; a kind left out fails to typecheck. */
function kindText(tr: Words): Record<ErrorKind, string> {
  return {
    locked: tr.error_locked,
    not_connected: tr.join_offline,
    connecting: tr.error_connecting,
    stopped: tr.error_stopped,
    timed_out: tr.join_timeout,
    lagged: tr.error_lagged,
    cancelled: tr.error_cancelled,
    invalid: tr.error_invalid,
    too_large: tr.error_too_large,
    unsupported: tr.error_unsupported,
    not_found: tr.error_not_found,
    not_ready: tr.error_not_ready,
    expired: tr.error_expired,
    not_saved: tr.file_missing,
    unsafe_type: tr.file_unsafe,
    no_app: tr.file_no_app,
    unknown_offer: tr.error_unknown_offer,
    no_downloads: tr.error_no_downloads,
    name_taken: tr.error_name_taken,
    drop_gone: tr.error_drop_gone,
    already_recording: tr.error_already_recording,
    not_recording: tr.error_not_recording,
    not_linking: tr.error_not_linking,
    // Worded by their reason; these are for a reason the UI does not know.
    failed: tr.error_failed,
    identity: tr.error_failed,
    internal: tr.error_failed,
  };
}

/** Every reason the core gives, in its events and in `failed` errors. */
function failText(tr: Words): Record<FailReason, string> {
  return {
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
    cancelled: tr.error_cancelled,
    source_unavailable: tr.error_source_unavailable,
    unauthorized: tr.error_unauthorized,
    server_error: tr.error_server,
    decrypt_failed: tr.error_decrypt_failed,
  };
}

function identityText(tr: Words): Record<IdentityKind, string> {
  return {
    missing: tr.identity_err_missing,
    exists: tr.identity_err_exists,
    weak_passphrase: tr.identity_err_short,
    wrong_passphrase: tr.identity_err_wrong_passphrase,
    corrupt: tr.identity_err_corrupt,
    invalid_mnemonic: tr.identity_err_bad_phrase,
    nickname_too_long: tr.identity_err_nickname,
  };
}

/** Looks `key` up among `words`' own keys only. */
function lookup<K extends string>(words: Record<K, string>, key: unknown): string | undefined {
  return typeof key === "string" && Object.prototype.hasOwnProperty.call(words, key) ? words[key as K] : undefined;
}

/**
 * A failure in the user's language: a command's error (`{kind, reason?,
 * message}`, from either platform), or a core reason an event carried
 * (`key_mismatch`). Anything else, and an `internal` error, is shown as it
 * says itself.
 */
export function reasonText(error: unknown): string {
  const tr = t();
  if (isCmdErrorShape(error)) {
    switch (error.kind) {
      case "failed":
        return lookup(failText(tr), error.reason) ?? tr.error_failed;
      case "identity":
        return lookup(identityText(tr), error.reason) ?? tr.error_failed;
      case "internal":
        return error.message || tr.error_failed;
      default:
        return kindText(tr)[error.kind];
    }
  }
  const raw = error instanceof Error ? error.message : String(error);
  return lookup(failText(tr), raw) ?? raw;
}

/** Whether a command failed only because the client is still starting. */
export function isConnecting(error: unknown): boolean {
  return isCmdErrorShape(error) && error.kind === "connecting";
}
