export const MAX_FRAME = 1024 * 1024;

export const MAX_STRING = 256 * 1024;

export const MAX_DEPTH = 32;

export const MAX_NODES = 16_384;

export const MAX_ENTRIES = 1024;

export type JsonObject = { [key: string]: JsonValue };

export type JsonValue = null | boolean | number | string | JsonValue[] | JsonObject;

export class WireError extends Error {
  readonly code: string;
  constructor(code: string) { super(code); this.code = code; }
}

/** Incrementally constructs bounded values; never JSON.parse an unchecked tree. */
export function boundedJson(bytes: Uint8Array): JsonValue {
  if (bytes.length === 0 || bytes.length > MAX_FRAME) throw new WireError("frame_size");
  let text: string;

  try { text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes); }
  catch { throw new WireError("invalid_utf8"); }

  let pos = 0;
  let nodes = 0;

  function whitespace(): void { while (/[\x20\t\r\n]/.test(text[pos] ?? "X")) pos++; }

  function string(): string {
    const start = pos++;

    while (pos < text.length) {
      const c = text[pos++];

      if (c === "\\") { pos++; continue; }

      if (c !== '"') continue;
      let value: string;

      try { value = JSON.parse(text.slice(start, pos)); }
      catch { throw new WireError("invalid_json"); }

      if (/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/u.test(value)) throw new WireError("invalid_json");

      if (Buffer.byteLength(value) > MAX_STRING) throw new WireError("limit_reached");

      return value;
    }

    throw new WireError("invalid_json");
  }

  function value(depth: number): JsonValue {
    whitespace();

    if (++nodes > MAX_NODES) throw new WireError("limit_reached");
    const c = text[pos];

    if (c === '"') return string();

    if (c === "[" || c === "{") {
      if (depth >= MAX_DEPTH) throw new WireError("limit_reached");
      const array = c === "[";
      const close = array ? "]" : "}";
      const list: JsonValue[] = [];
      const object: JsonObject = Object.create(null);
      let count = 0;
      pos++; whitespace();

      if (text[pos] === close) {
        pos++;

        return array ? list : object;
      }

      while (pos < text.length) {
        // Rust's sequence visitor sees the next bounded value before checking count.
        if (array) {
          const item = value(depth + 1);

          if (count === MAX_ENTRIES) throw new WireError("limit_reached");
          list.push(item);
        } else {
          whitespace();

          if (text[pos] !== '"') throw new WireError("invalid_json");
          const key = string();

          if (count === MAX_ENTRIES) throw new WireError("limit_reached");

          if (Object.hasOwn(object, key)) throw new WireError("duplicate_key");
          whitespace();

          if (text[pos++] !== ":") throw new WireError("invalid_json");
          object[key] = value(depth + 1);
        }

        count++; whitespace();
        const next = text[pos++];

        if (next === close) return array ? list : object;

        if (next !== ",") throw new WireError("invalid_json");

        if (array) {
          // Like Rust's sequence access: a comma must be followed by a value, checked before
          // that value is requested (and counted against the node budget).
          whitespace();

          if (pos >= text.length || text[pos] === close) throw new WireError("invalid_json");
        }
      }

      throw new WireError("invalid_json");
    }

    for (const [token, result] of [["true", true], ["false", false], ["null", null]] as const) {
      if (text.startsWith(token, pos)) {
        pos += token.length;

        return result;
      }
    }

    const number = /^-?(?:0|[1-9][0-9]*)/.exec(text.slice(pos))?.[0];

    if (!number || number === "-0") throw new WireError("invalid_json");
    pos += number.length;

    if (!Number.isSafeInteger(Number(number))) throw new WireError("invalid_json");

    return Number(number);
  }

  const result = value(0);
  whitespace();

  if (pos !== text.length) throw new WireError("invalid_json");

  return result;
}
