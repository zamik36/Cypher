/**
 * Why a command failed, as both platforms report it: the core's `CmdError`
 * (`crates/cypher-core/src/errors.rs`), whose tests check that the unions
 * below name exactly what it serializes.
 */

/** What went wrong; `failed` and `identity` say more in `reason`. */
export type ErrorKind =
  | "locked"
  | "not_connected"
  | "connecting"
  | "stopped"
  | "timed_out"
  | "lagged"
  | "cancelled"
  | "invalid"
  | "too_large"
  | "unsupported"
  | "not_found"
  | "not_ready"
  | "expired"
  | "not_saved"
  | "unsafe_type"
  | "no_app"
  | "unknown_offer"
  | "no_downloads"
  | "name_taken"
  | "drop_gone"
  | "already_recording"
  | "not_recording"
  | "not_linking"
  | "failed"
  | "identity"
  | "internal";

/** Why the core failed something; also the reason in its events. */
export type FailReason =
  | "not_found"
  | "invalid_link"
  | "self_link"
  | "invalid_keys"
  | "timeout"
  | "offline"
  | "rejected"
  | "cancelled"
  | "source_unavailable"
  | "corrupted"
  | "unauthorized"
  | "server_error"
  | "decrypt_failed"
  | "key_mismatch"
  | "storage_failed"
  | "update_required";

/** What is wrong with the identity file or the recovery phrase. */
export type IdentityKind =
  "missing" | "exists" | "weak_passphrase" | "wrong_passphrase" | "corrupt" | "invalid_mnemonic" | "nickname_too_long";

/** A failure as it crosses from the platform: plain data. */
export interface CmdErrorShape {
  kind: ErrorKind;
  reason?: FailReason | IdentityKind | undefined;
  /** English, for logs and for what the UI has no words for. */
  message: string;
}

/** A command's failure, thrown to the UI. */
export class CommandError extends Error implements CmdErrorShape {
  readonly kind: ErrorKind;
  readonly reason: FailReason | IdentityKind | undefined;

  constructor(shape: CmdErrorShape) {
    super(shape.message);
    this.name = "CommandError";
    this.kind = shape.kind;
    this.reason = shape.reason;
  }
}

/** Builds a failure of `kind`, with its `reason`, told by `message`. */
export function cmdError(kind: ErrorKind, message: string, reason?: FailReason | IdentityKind): CmdErrorShape {
  return reason === undefined ? { kind, message } : { kind, reason, message };
}

/** Whether `value` has the shape of a platform's failure. */
export function isCmdErrorShape(value: unknown): value is CmdErrorShape {
  if (typeof value !== "object" || value === null) return false;
  const v = value as Record<string, unknown>;
  return typeof v["kind"] === "string" && typeof v["message"] === "string";
}

/**
 * `e` as the shape the platforms report: kept when it has it, anything else
 * (an exception, a platform's own text) as an `internal` failure.
 */
export function toCmdErrorShape(e: unknown): CmdErrorShape {
  if (isCmdErrorShape(e)) {
    return e.reason === undefined
      ? { kind: e.kind, message: e.message }
      : { kind: e.kind, reason: e.reason, message: e.message };
  }
  return { kind: "internal", message: e instanceof Error ? e.message : String(e) };
}

/** `e` as a thrown failure; see `toCmdErrorShape`. */
export function toCommandError(e: unknown): CommandError {
  return e instanceof CommandError ? e : new CommandError(toCmdErrorShape(e));
}
