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

/** Orders strings by UTF-16 code unit: the order of `Array.prototype.sort` without a comparator. */
export function compareCodeUnits(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

const UTF8 = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

const WHITESPACE = /[\x20\t\r\n]/;

const LONE_SURROGATE = /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/u;

const INTEGER = /^-?(?:0|[1-9][0-9]*)/;

const LITERALS = [["true", true], ["false", false], ["null", null]] as const;

class Parser {
  private readonly text: string;
  private pos = 0;
  private nodes = 0;
  constructor(text: string) { this.text = text; }

  document(): JsonValue {
    const result = this.value(0);
    this.whitespace();

    if (this.pos !== this.text.length) throw new WireError("invalid_json");

    return result;
  }

  private whitespace(): void {
    while (WHITESPACE.test(this.text[this.pos] ?? "X")) this.pos++;
  }

  private string(): string {
    const start = this.pos++;

    while (this.pos < this.text.length) {
      const c = this.text[this.pos++];

      if (c === "\\") { this.pos++; continue; }

      if (c !== '"') continue;
      let value: string;

      try { value = JSON.parse(this.text.slice(start, this.pos)); }
      catch { throw new WireError("invalid_json"); }

      if (LONE_SURROGATE.test(value)) throw new WireError("invalid_json");

      if (Buffer.byteLength(value) > MAX_STRING) throw new WireError("limit_reached");

      return value;
    }

    throw new WireError("invalid_json");
  }

  private value(depth: number): JsonValue {
    this.whitespace();

    if (++this.nodes > MAX_NODES) throw new WireError("limit_reached");
    const c = this.text[this.pos];

    if (c === '"') return this.string();

    if (c !== "[" && c !== "{") return this.scalar();

    if (depth >= MAX_DEPTH) throw new WireError("limit_reached");
    this.pos++; this.whitespace();

    return c === "[" ? this.array(depth) : this.object(depth);
  }

  /** Consumes `close` when it is the next character, ending an empty container. */
  private empty(close: string): boolean {
    if (this.text[this.pos] !== close) return false;
    this.pos++;

    return true;
  }

  /** Consumes the separator after a member: true at `close`, false at a comma. */
  private closes(close: string): boolean {
    this.whitespace();
    const next = this.text[this.pos++];

    if (next === close) return true;

    if (next !== ",") throw new WireError("invalid_json");

    return false;
  }

  private array(depth: number): JsonValue[] {
    const list: JsonValue[] = [];

    if (this.empty("]")) return list;

    while (this.pos < this.text.length) {
      const item = this.value(depth + 1);

      if (list.length === MAX_ENTRIES) throw new WireError("limit_reached");
      list.push(item);

      if (this.closes("]")) return list;
      this.whitespace();

      if (this.text[this.pos] === "]") throw new WireError("invalid_json");
    }

    throw new WireError("invalid_json");
  }

  private object(depth: number): JsonObject {
    const object: JsonObject = Object.create(null);
    let count = 0;

    if (this.empty("}")) return object;

    while (this.pos < this.text.length) {
      this.whitespace();

      if (this.text[this.pos] !== '"') throw new WireError("invalid_json");
      const key = this.string();

      if (count === MAX_ENTRIES) throw new WireError("limit_reached");

      if (Object.hasOwn(object, key)) throw new WireError("duplicate_key");
      this.whitespace();

      if (this.text[this.pos++] !== ":") throw new WireError("invalid_json");
      object[key] = this.value(depth + 1);
      count++;

      if (this.closes("}")) return object;
    }

    throw new WireError("invalid_json");
  }

  private scalar(): JsonValue {
    for (const [token, result] of LITERALS) {
      if (this.text.startsWith(token, this.pos)) {
        this.pos += token.length;

        return result;
      }
    }

    const number = INTEGER.exec(this.text.slice(this.pos))?.[0];

    if (!number || number === "-0") throw new WireError("invalid_json");
    this.pos += number.length;

    if (!Number.isSafeInteger(Number(number))) throw new WireError("invalid_json");

    return Number(number);
  }
}

export function boundedJson(bytes: Uint8Array): JsonValue {
  if (bytes.length === 0 || bytes.length > MAX_FRAME) throw new WireError("frame_size");
  let text: string;

  try { text = UTF8.decode(bytes); }
  catch { throw new WireError("invalid_utf8"); }

  return new Parser(text).document();
}
