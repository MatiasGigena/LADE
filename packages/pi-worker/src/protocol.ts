import schema from "../../../contracts/v1/schema.json" with { type: "json" };
import type { Envelope } from "./dto.ts";
import { boundedJson, WireError } from "./json.ts";
import type { JsonObject, JsonValue } from "./json.ts";

export const VERSION = 1;

export const BUNDLE = "0.1.0-imp001";

export const MAX_CHUNK = 64 * 1024;

export const MAX_TRANSFER = 8 * 1024 * 1024;

export const MAX_TRANSFERS = 2;

interface Schema {
  $ref?: string;
  oneOf?: Schema[];
  const?: JsonValue;
  enum?: JsonValue[];
  type?: string;
  properties?: Record<string, Schema>;
  format?: string;
}

const definitions: Record<string, Schema> = schema.$defs;

export function record(value: JsonValue): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isString(value: JsonValue): value is string {
  return typeof value === "string";
}

function matches(value: JsonValue, definition: Schema): boolean {
  if (definition.$ref) {
    const referenced = definitions[definition.$ref.slice("#/$defs/".length)];

    return referenced !== undefined && matches(value, referenced);
  }

  if (definition.oneOf) return definition.oneOf.filter(choice => matches(value, choice)).length === 1;

  if (Object.hasOwn(definition, "const")) return value === definition.const;

  if (definition.enum) return definition.enum.includes(value);

  if (definition.type === "object") {
    const properties = definition.properties;

    return properties !== undefined && record(value) && Object.keys(value).length === Object.keys(properties).length &&
      Object.entries(properties).every(([key, property]) => {
        if (!Object.hasOwn(value, key)) return false;
        const entry = value[key];

        return entry !== undefined && matches(entry, property);
      });
  }

  if (definition.type !== "string" || !isString(value)) return false;

  switch (definition.format) {
    case "uuid-v4": return /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value);
    case "u64": return /^(?:0|[1-9][0-9]{0,19})$/.test(value) && BigInt(value) <= 18446744073709551615n;
    case "sha256": return /^[0-9a-f]{64}$/.test(value);
    case "chunk-hex": return value.length > 0 && value.length <= 2 * MAX_CHUNK && value.length % 2 === 0 && /^[0-9a-f]+$/.test(value);
    default: return false;
  }
}

export function validate(value: JsonValue): asserts value is Envelope {
  if (!record(value) || value.protocol !== VERSION || value.bundle !== BUNDLE) throw new WireError("version_mismatch");

  if (!matches(value, schema)) throw new WireError("invalid_schema");
}

export function decode(bytes: Uint8Array): Envelope {
  const value = boundedJson(bytes);
  validate(value);

  return value;
}
