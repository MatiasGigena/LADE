import workerSchema from "./worker-schema.json" with { type: "json" };
import type { WorkerEnvelope } from "../dto.ts";
import { WireError } from "../json.ts";
import type { JsonValue } from "../json.ts";
import { matches, versioned } from "../schema.ts";
import type { Contract } from "../schema.ts";

const worker: Contract = { root: workerSchema, definitions: workerSchema.$defs };

export function validateWorker(value: JsonValue): asserts value is WorkerEnvelope {
  if (!matches(versioned(value), worker.root, worker)) throw new WireError("invalid_schema");
}
