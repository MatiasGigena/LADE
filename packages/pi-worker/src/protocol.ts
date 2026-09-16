import schema from "../../../contracts/v1/schema.json" with { type: "json" };
import type { Envelope } from "./dto.ts";
import { boundedJson, WireError } from "./json.ts";

export const VERSION = 1;
export const BUNDLE = "0.1.0-imp001";
export const MAX_CHUNK = 64 * 1024;
export const MAX_TRANSFER = 8 * 1024 * 1024;
export const MAX_TRANSFERS = 2;

interface Schema {
  $ref?: string;
  oneOf?: Schema[];
  const?: unknown;
  enum?: unknown[];
  type?: string;
  properties?: Record<string, Schema>;
  format?: string;
}
const definitions: Record<string, Schema> = schema.$defs;

export function record(v: unknown): v is Record<string, unknown> {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}
function matches(v: unknown, s: Schema): boolean {
  if (s.$ref) {
    const definition = definitions[s.$ref.slice("#/$defs/".length)];
    return definition !== undefined && matches(v, definition);
  }
  if (s.oneOf) return s.oneOf.filter(choice => matches(v, choice)).length === 1;
  if (Object.hasOwn(s, "const")) return v === s.const;
  if (s.enum) return s.enum.includes(v);
  if (s.type === "object") {
    const props = s.properties;
    return props !== undefined && record(v) && Object.keys(v).length === Object.keys(props).length &&
      Object.entries(props).every(([key, def]) => Object.hasOwn(v, key) && matches(v[key], def));
  }
  if (s.type !== "string" || typeof v !== "string") return false;
  switch (s.format) {
    case "uuid-v4": return /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(v);
    case "u64": return /^(?:0|[1-9][0-9]{0,19})$/.test(v) && BigInt(v) <= 18446744073709551615n;
    case "sha256": return /^[0-9a-f]{64}$/.test(v);
    case "chunk-hex": return v.length > 0 && v.length <= 2 * MAX_CHUNK && v.length % 2 === 0 && /^[0-9a-f]+$/.test(v);
    default: return false;
  }
}
export function validate(value: unknown): asserts value is Envelope {
  if (!record(value) || value.protocol !== VERSION || value.bundle !== BUNDLE) throw new WireError("version_mismatch");
  if (!matches(value, schema)) throw new WireError("invalid_schema");
}
export function decode(bytes: Uint8Array): Envelope {
  const value = boundedJson(bytes);
  validate(value);
  return value;
}
