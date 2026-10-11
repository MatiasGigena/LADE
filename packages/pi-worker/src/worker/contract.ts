// The worker-side validator over the package-local generated copy of the worker schema
// (identical to contracts/v1/worker-schema.json; scripts/generate-schema.py writes both).
import workerSchema from "./worker-schema.json" with { type: "json" };
import type { WorkerEnvelope } from "../dto.ts";
import { WireError } from "../json.ts";
import type { JsonValue } from "../json.ts";
import { matches, versioned } from "../schema.ts";
import type { Contract } from "../schema.ts";

const worker: Contract = { root: workerSchema, definitions: workerSchema.$defs };

/** Worker contract: exact versions, then the closed schema. */
export function validateWorker(value: JsonValue): asserts value is WorkerEnvelope {
  if (!matches(versioned(value), worker.root, worker)) throw new WireError("invalid_schema");
}
