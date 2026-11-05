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

const MAX_PATH_BYTES = 4096;

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

export function isRecord(value: JsonValue | undefined): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function isString<Value>(value: Value): value is Value & string {
  return typeof value === "string";
}

function isBoolean(value: JsonValue): value is boolean {
  return typeof value === "boolean";
}

const U64_MAX = 2n ** 64n - 1n;

const MAX_PAGE_LIMIT = 200n;

const MAX_READ_LIMIT = 256n * 1024n;

const UNIX_EPOCH_YEAR = 1970;

const DAYS_IN_MONTH = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

const DECIMAL = /^(?:0|[1-9][0-9]{0,19})$/;

const TIMESTAMP = /^([0-9]{4})-([0-9]{2})-([0-9]{2})T([0-9]{2}):([0-9]{2}):([0-9]{2})\.[0-9]{3}Z$/;

const BASE64 = /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/;

const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

const SHA256_HEX = /^[0-9a-f]{64}$/;

const SHA1_HEX = /^[0-9a-f]{40}$/;

const HEX = /^[0-9a-f]+$/;

function decimal(value: string): bigint | undefined {
  return DECIMAL.test(value) && BigInt(value) <= U64_MAX ? BigInt(value) : undefined;
}

function daysIn(year: number, month: number): number | undefined {
  const leap = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);

  return month === 2 && leap ? 29 : DAYS_IN_MONTH[month - 1];
}

function timestamp(value: string): boolean {
  const parts = TIMESTAMP.exec(value);

  if (!parts) return false;
  const [year, month, day, hour, minute, second] = parts.slice(1).map(Number);

  if (year === undefined || month === undefined || day === undefined || hour === undefined || minute === undefined || second === undefined) return false;
  const days = daysIn(year, month);

  return days !== undefined && year >= UNIX_EPOCH_YEAR && day >= 1 && day <= days && hour < 24 && minute < 60 && second < 60;
}

function inRange(value: bigint | undefined, max: bigint): boolean {
  return value !== undefined && value >= 1n && value <= max;
}

function pathBytes(value: string): boolean {
  if (!BASE64.test(value)) return false;
  const bytes = Buffer.from(value, "base64");

  return bytes.toString("base64") === value && bytes.length >= 1 && bytes.length <= MAX_PATH_BYTES;
}

function chunkHex(value: string): boolean {
  return value.length > 0 && value.length <= 2 * MAX_CHUNK && value.length % 2 === 0 && HEX.test(value);
}

const FORMATS = new Map<string, (value: string) => boolean>([
  ["uuid-v4", value => UUID_V4.test(value)],
  ["u64", value => decimal(value) !== undefined],
  ["page-limit", value => inRange(decimal(value), MAX_PAGE_LIMIT)],
  ["read-limit", value => inRange(decimal(value), MAX_READ_LIMIT)],
  ["sha256", value => SHA256_HEX.test(value)],
  ["capability", value => SHA256_HEX.test(value)],
  ["git-sha1", value => SHA1_HEX.test(value)],
  ["chunk-hex", chunkHex],
  ["timestamp", timestamp],
  ["path-b64", pathBytes],
]);

function matchesFormat(value: string, kind: string): boolean {
  return FORMATS.get(kind)?.(value) ?? false;
}

function codePoints(value: string): number {
  let count = 0;

  for (const _ of value) count++;

  return count;
}

function matchesObject(value: JsonValue, definition: Schema, contract: Contract): boolean {
  const properties = definition.properties;
  const required = definition.required;

  return properties !== undefined && required !== undefined && isRecord(value) &&
    required.every(key => Object.hasOwn(value, key)) &&
    Object.keys(value).every(key => {
      const property = Object.hasOwn(properties, key) ? properties[key] : undefined;
      const entry = value[key];

      return property !== undefined && entry !== undefined && matches(entry, property, contract);
    });
}

function matchesArray(value: JsonValue, definition: Schema, contract: Contract): boolean {
  const items = definition.items;

  return items !== undefined && definition.maxItems !== undefined && Array.isArray(value) &&
    value.length <= definition.maxItems && value.length >= (definition.minItems ?? 0) &&
    value.every(item => matches(item, items, contract));
}

function matchesString(value: JsonValue, definition: Schema): boolean {
  if (!isString(value)) return false;
  const { minLength, maxLength } = definition;

  if (minLength !== undefined || maxLength !== undefined) {
    const count = codePoints(value);

    if (count < (minLength ?? 0) || (maxLength !== undefined && count > maxLength)) return false;
  }

  return definition.format === undefined || matchesFormat(value, definition.format);
}

function matchesType(value: JsonValue, definition: Schema, contract: Contract): boolean {
  switch (definition.type) {
    case "object": return matchesObject(value, definition, contract);
    case "array": return matchesArray(value, definition, contract);
    case "string": return matchesString(value, definition);
    case "boolean": return isBoolean(value);
    default: return false;
  }
}

export function matches(value: JsonValue, definition: Schema, contract: Contract): boolean {
  if (definition.$ref) {
    const referenced = contract.definitions[definition.$ref.slice("#/$defs/".length)];

    return referenced !== undefined && matches(value, referenced, contract);
  }

  if (definition.oneOf) return definition.oneOf.filter(choice => matches(value, choice, contract)).length === 1;

  if (Object.hasOwn(definition, "const")) return value === definition.const;

  if (definition.enum) return definition.enum.includes(value);

  return matchesType(value, definition, contract);
}

export function versioned(value: JsonValue): JsonObject {
  if (!isRecord(value) || value.protocol !== VERSION || value.bundle !== BUNDLE) throw new WireError("version_mismatch");

  return value;
}

// Every service-to-worker kind, checked against the generated union: a new kind fails type-checking until it is listed.
const SERVICE_KINDS = {
  "worker.start": true,
  "input.deliver": true,
  "session.abort": true,
  "worker.shutdown": true,
  "session.command": true,
  "ui.respond": true,
} as const satisfies Record<ServiceToWorker["kind"], true>;

export function isServiceToWorker(body: WorkerBody): body is ServiceToWorker {
  return Object.hasOwn(SERVICE_KINDS, body.kind);
}

export function isWorkerToService(body: WorkerBody): body is WorkerToService {
  return !isServiceToWorker(body);
}

export function parseId(text: string): Id {
  if (!matchesFormat(text, "uuid-v4")) throw new WireError("invalid_schema");

  // SAFETY: the uuid-v4 format check above is the brand's defining invariant.
  return text as Id;
}

export function newId(): Id {
  return parseId(randomUUID());
}

export function counter(value: number): Counter {
  const text = String(value);

  if (!Number.isSafeInteger(value) || value < 0 || !matchesFormat(text, "u64")) throw new WireError("invalid_schema");

  // SAFETY: the u64 format check above is the brand's defining invariant.
  return text as Counter;
}
