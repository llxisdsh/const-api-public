const storagePrefix = "const-api.finance-attempt.v1";
const attemptLifetimeMs = 7 * 24 * 60 * 60 * 1000;

type StoredAttempt = {
  fingerprint: string;
  requestID: string;
  createdAt: number;
};

export type FinanceAttempt = {
  storageKey: string;
  requestID: string;
};

function randomRequestID(prefix: string): string {
  const value = typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `${Date.now()}-${Math.random().toString(16).slice(2)}`;
  return `${prefix}-${value}`;
}

function stableJSON(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(stableJSON).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.entries(value as Record<string, unknown>)
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([key, entry]) => `${JSON.stringify(key)}:${stableJSON(entry)}`)
      .join(",")}}`;
  }
  return JSON.stringify(value) ?? "null";
}

async function fingerprint(value: unknown): Promise<string> {
  const payload = new TextEncoder().encode(stableJSON(value));
  if (typeof crypto !== "undefined" && crypto.subtle) {
    const digest = await crypto.subtle.digest("SHA-256", payload);
    return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
  }
  // The fallback is only an idempotency comparison key; it never authorizes
  // an operation, and the original (possibly sensitive) payload is not stored.
  let left = 0x811c9dc5;
  let right = 0x9e3779b9;
  for (const byte of payload) {
    left = Math.imul(left ^ byte, 0x01000193);
    right = Math.imul(right ^ byte, 0x85ebca6b);
  }
  return `${(left >>> 0).toString(16).padStart(8, "0")}${(right >>> 0).toString(16).padStart(8, "0")}`;
}

export async function beginFinanceAttempt(
  scope: string,
  operation: string,
  payload: unknown,
): Promise<FinanceAttempt> {
  const storageKey = `${storagePrefix}:${encodeURIComponent(scope)}:${operation}`;
  const payloadFingerprint = await fingerprint(payload);
  try {
    const raw = localStorage.getItem(storageKey);
    if (raw) {
      const current = JSON.parse(raw) as Partial<StoredAttempt>;
      if (
        current.fingerprint === payloadFingerprint
        && typeof current.requestID === "string"
        && typeof current.createdAt === "number"
        && Date.now() - current.createdAt <= attemptLifetimeMs
      ) {
        return { storageKey, requestID: current.requestID };
      }
    }
    const requestID = randomRequestID(operation);
    localStorage.setItem(storageKey, JSON.stringify({
      fingerprint: payloadFingerprint,
      requestID,
      createdAt: Date.now(),
    } satisfies StoredAttempt));
    return { storageKey, requestID };
  } catch {
    return { storageKey: "", requestID: randomRequestID(operation) };
  }
}

export function completeFinanceAttempt(attempt: FinanceAttempt): void {
  if (!attempt.storageKey) return;
  try {
    const raw = localStorage.getItem(attempt.storageKey);
    if (!raw) return;
    const current = JSON.parse(raw) as Partial<StoredAttempt>;
    if (current.requestID === attempt.requestID) {
      localStorage.removeItem(attempt.storageKey);
    }
  } catch {
    // Storage is an optimization for retry safety; server idempotency remains
    // authoritative when storage is unavailable.
  }
}

export function discardFinanceAttempt(attempt: FinanceAttempt): void {
  completeFinanceAttempt(attempt);
}
