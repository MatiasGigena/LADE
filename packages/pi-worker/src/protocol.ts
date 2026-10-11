import clientSchema from "../../../contracts/v1/schema.json" with { type: "json" };
import workerSchema from "../../../contracts/v1/worker-schema.json" with { type: "json" };
import type { Envelope, WorkerEnvelope } from "./dto.ts";
import { boundedJson, WireError } from "./json.ts";
import type { JsonValue } from "./json.ts";
import { matches, versioned } from "./schema.ts";
import type { Contract } from "./schema.ts";

export { BUNDLE, counter, isServiceToWorker, isWorkerToService, MAX_CHUNK, MAX_ITEM_TEXT_BYTES, MAX_PATH_BYTES, MAX_TRANSFER, MAX_TRANSFERS, newId, parseId, record, VERSION } from "./schema.ts";

const client: Contract = { root: clientSchema, definitions: clientSchema.$defs };

const worker: Contract = { root: workerSchema, definitions: workerSchema.$defs };

/** Client contract (`contracts/v1/schema.json`). */
export function validate(value: JsonValue): asserts value is Envelope {
  if (!matches(versioned(value), client.root, client)) throw new WireError("invalid_schema");
}

/** Worker contract (`contracts/v1/worker-schema.json`). */
export function validateWorker(value: JsonValue): asserts value is WorkerEnvelope {
  if (!matches(versioned(value), worker.root, worker)) throw new WireError("invalid_schema");
}

/** Client-contract frame body: bounded parse, exact versions, closed schema. */
export function decode(bytes: Uint8Array): Envelope {
  const value = boundedJson(bytes);
  validate(value);

  return value;
}

/** Worker-contract frame body (without its LF): bounded parse, exact versions, closed schema. */
export function decodeWorkerFrame(bytes: Uint8Array): WorkerEnvelope {
  const value = boundedJson(bytes);
  validateWorker(value);

  return value;
}

