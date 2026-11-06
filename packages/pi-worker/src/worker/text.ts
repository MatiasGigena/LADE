/** One-line, character-bounded text for frames: titles, labels, dialog fields and error messages. */

const MAX_ERROR_MESSAGE = 512;

export function isHighSurrogate(code: number): boolean {
  return code >= 0xd800 && code <= 0xdbff;
}

/** At most `max` UTF-16 code units of `text`, never ending in half a surrogate pair. */
export function prefix(text: string, max: number): string {
  if (text.length <= max) return text;

  return text.slice(0, isHighSurrogate(text.charCodeAt(max - 1)) ? max - 1 : max);
}

/** `text` on one line (whitespace runs collapsed) and at most `max` characters, ending in an ellipsis when cut. */
export function clip(text: string, max: number): string {
  const line = text.replace(/\s+/g, " ").trim();

  return line.length > max ? `${prefix(line, max - 1)}…` : line;
}

/** A diagnostic that may leave the worker: control characters become spaces, token-like runs are masked, and it fits `worker.error`. */
export function safeMessage(text: string): string {
  const line = text.replace(/\p{Cc}+/gu, " ").replace(/[A-Za-z0-9_\-+/=.]{32,}/g, "[redacted]").trim();

  return line.length > MAX_ERROR_MESSAGE ? `${prefix(line, MAX_ERROR_MESSAGE - 1)}…` : line;
}
