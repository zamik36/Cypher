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

/** A kind that carries no reason. */
export type PlainKind = Exclude<ErrorKind, "failed" | "identity">;

/**
 * A failure as it crosses from the platform, plain data: `failed` and
 * `identity` say why in `reason`, the other kinds have none. `message` is
 * English, for logs and for what the UI has no words for.
 */
export type CmdErrorShape =
  | { kind: "failed"; reason: FailReason; message: string }
  | { kind: "identity"; reason: IdentityKind; message: string }
  | { kind: PlainKind; message: string };

/** A command's failure, thrown to the UI. */
export class CommandError extends Error {
  readonly kind: ErrorKind;
  readonly reason: FailReason | IdentityKind | undefined;
  /** The failure as the platform reported it. */
  readonly shape: CmdErrorShape;

  constructor(shape: CmdErrorShape) {
    super(shape.message);
    this.name = "CommandError";
    this.kind = shape.kind;
    this.reason = "reason" in shape ? shape.reason : undefined;
    this.shape = shape;
  }
}

/** Whether `value` has the shape of a platform's failure. */
export function isCmdErrorShape(value: unknown): value is CmdErrorShape {
  if (typeof value !== "object" || value === null) return false;
  const v = value as Record<string, unknown>;
  const reasoned = v["kind"] === "failed" || v["kind"] === "identity";
  return (
    typeof v["kind"] === "string" && typeof v["message"] === "string" && (!reasoned || typeof v["reason"] === "string")
  );
}

/**
 * `e` as the shape the platforms report: kept when it has it, anything else
 * (an exception, a platform's own text) as an `internal` failure.
 */
export function toCmdErrorShape(e: unknown): CmdErrorShape {
  if (e instanceof CommandError) return { ...e.shape };
  if (isCmdErrorShape(e)) return { ...e };
  return { kind: "internal", message: e instanceof Error ? e.message : String(e) };
}

/** `e` as a thrown failure; see `toCmdErrorShape`. */
export function toCommandError(e: unknown): CommandError {
  return e instanceof CommandError ? e : new CommandError(toCmdErrorShape(e));
}
