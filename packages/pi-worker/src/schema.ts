import { randomUUID } from "node:crypto";
import type { Counter, Id, ServiceToWorker, WorkerBody, WorkerToService } from "./dto.ts";
import { WireError } from "./json.ts";
import type { JsonObject, JsonValue } from "./json.ts";

export const VERSION = 1;

export const BUNDLE = "0.2.0";

export const MAX_CHUNK = 64 * 1024;

export const MAX_TRANSFER = 8 * 1024 * 1024;

export const MAX_TRANSFERS = 2;

export const MAX_ITEM_TEXT_BYTES = 192 * 1024;

export const MAX_PATH_BYTES = 4096;

export interface Schema {
  $ref?: string;
  oneOf?: Schema[];
  const?: JsonValue;
  enum?: JsonValue[];
  type?: string;
  properties?: Record<string, Schema>;
  required?: string[];
  items?: Schema;
  minItems?: number;
  maxItems?: number;
  minLength?: number;
  maxLength?: number;
  format?: string;
}

export interface Contract { root: Schema; definitions: Record<string, Schema> }

export function record(value: JsonValue): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isString(value: JsonValue): value is string {
  return typeof value === "string";
}

function isBoolean(value: JsonValue): value is boolean {
  return typeof value === "boolean";
}

function decimal(value: string): bigint | undefined {
  return /^(?:0|[1-9][0-9]{0,19})$/.test(value) && BigInt(value) <= 18446744073709551615n ? BigInt(value) : undefined;
}

function timestamp(value: string): boolean {
  const parts = /^([0-9]{4})-([0-9]{2})-([0-9]{2})T([0-9]{2}):([0-9]{2}):([0-9]{2})\.[0-9]{3}Z$/.exec(value);

  if (!parts) return false;
  const [year, month, day, hour, minute, second] = parts.slice(1).map(Number);

  if (year === undefined || month === undefined || day === undefined || hour === undefined || minute === undefined || second === undefined) return false;
  const leap = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
  const days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][month - 1];

  return days !== undefined && year >= 1970 && day >= 1 && day <= days && hour < 24 && minute < 60 && second < 60;
}

function within(value: bigint | undefined, max: bigint): boolean {
  return value !== undefined && value >= 1n && value <= max;
}

function pathBytes(value: string): boolean {
  if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(value)) return false;
  const bytes = Buffer.from(value, "base64");

  return bytes.toString("base64") === value && bytes.length >= 1 && bytes.length <= MAX_PATH_BYTES;
}

export function format(value: string, kind: string): boolean {
  switch (kind) {
    case "uuid-v4": return /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value);
    case "u64": return decimal(value) !== undefined;
    case "page-limit": return within(decimal(value), 200n);
    case "read-limit": return within(decimal(value), 262144n);
    case "sha256":
    case "capability": return /^[0-9a-f]{64}$/.test(value);
    case "git-sha1": return /^[0-9a-f]{40}$/.test(value);
    case "chunk-hex": return value.length > 0 && value.length <= 2 * MAX_CHUNK && value.length % 2 === 0 && /^[0-9a-f]+$/.test(value);
    case "timestamp": return timestamp(value);
    case "path-b64": return pathBytes(value);
    default: return false;
  }
}

function codePoints(value: string): number {
  let count = 0;

  for (const _ of value) count++;

  return count;
}

export function matches(value: JsonValue, definition: Schema, contract: Contract): boolean {
  if (definition.$ref) {
    const referenced = contract.definitions[definition.$ref.slice("#/$defs/".length)];

    return referenced !== undefined && matches(value, referenced, contract);
  }

  if (definition.oneOf) return definition.oneOf.filter(choice => matches(value, choice, contract)).length === 1;

  if (Object.hasOwn(definition, "const")) return value === definition.const;

  if (definition.enum) return definition.enum.includes(value);

  switch (definition.type) {
    case "object": {
      const properties = definition.properties;
      const required = definition.required;

      return properties !== undefined && required !== undefined && record(value) &&
        required.every(key => Object.hasOwn(value, key)) &&
        Object.keys(value).every(key => {
          const property = Object.hasOwn(properties, key) ? properties[key] : undefined;
          const entry = value[key];

          return property !== undefined && entry !== undefined && matches(entry, property, contract);
        });
    }

    case "array": {
      const items = definition.items;

      return items !== undefined && definition.maxItems !== undefined && Array.isArray(value) &&
        value.length <= definition.maxItems && value.length >= (definition.minItems ?? 0) &&
        value.every(item => matches(item, items, contract));
    }

    case "string": {
      if (!isString(value)) return false;
      const { minLength, maxLength } = definition;

      if (minLength !== undefined || maxLength !== undefined) {
        const count = codePoints(value);

        if (count < (minLength ?? 0) || (maxLength !== undefined && count > maxLength)) return false;
      }

      return definition.format === undefined || format(value, definition.format);
    }

    case "boolean": return isBoolean(value);
    default: return false;
  }
}

export function versioned(value: JsonValue): JsonObject {
  if (!record(value) || value.protocol !== VERSION || value.bundle !== BUNDLE) throw new WireError("version_mismatch");

  return value;
}

export function isServiceToWorker(body: WorkerBody): body is ServiceToWorker {
  return body.kind === "worker.start" || body.kind === "input.deliver" || body.kind === "session.abort" || body.kind === "worker.shutdown" ||
    body.kind === "session.command" || body.kind === "ui.respond";
}

export function isWorkerToService(body: WorkerBody): body is WorkerToService {
  return !isServiceToWorker(body);
}

export function parseId(text: string): Id {
  if (!format(text, "uuid-v4")) throw new WireError("invalid_schema");

  // SAFETY: the uuid-v4 format check above is the brand's defining invariant.
  return text as Id;
}

export function newId(): Id {
  return parseId(randomUUID());
}

export function counter(value: number): Counter {
  const text = String(value);

  if (!Number.isSafeInteger(value) || value < 0 || !format(text, "u64")) throw new WireError("invalid_schema");

  // SAFETY: the u64 format check above is the brand's defining invariant.
  return text as Counter;
}
