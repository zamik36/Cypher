/**
 * Tor bridge lines as the user pasted them. Only plain bridges work in this
 * build (`[Bridge] IP:port FINGERPRINT`): pluggable transports such as obfs4
 * need a transport binary the app does not ship.
 */
const PLAIN = /^(?:bridge\s+)?(?:\d{1,3}(?:\.\d{1,3}){3}|\[[0-9a-f:.]+\]):\d{1,5}\s+\$?[0-9a-f]{40}(?:\s+\S+)*$/i;
/** A transport name in front of the address: `obfs4 1.2.3.4:443 ...`. */
const TRANSPORT = /^(?:bridge\s+)?[a-z][a-z0-9_-]*\s+\S+:\d+/i;

export type BridgeProblem = "transport" | "format";

export interface BridgeCheck {
  /** The non-empty lines, trimmed, without repeats. */
  lines: string[];
  /** The first line that cannot be used, counted from 1, and why. */
  problem: { line: number; reason: BridgeProblem } | null;
}

export function checkBridges(text: string): BridgeCheck {
  const lines: string[] = [];
  let problem: BridgeCheck["problem"] = null;
  text.split(/\r?\n/).forEach((raw, i) => {
    const line = raw.trim().replace(/\s+/g, " ");
    if (!line || lines.includes(line)) return;
    lines.push(line);
    if (problem || PLAIN.test(line)) return;
    problem = { line: i + 1, reason: TRANSPORT.test(line) ? "transport" : "format" };
  });
  return { lines, problem };
}
