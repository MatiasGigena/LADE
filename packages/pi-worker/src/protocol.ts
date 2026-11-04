import clientSchema from "../../../contracts/v1/schema.json" with { type: "json" };
import type { Envelope } from "./dto.ts";
import { boundedJson, WireError } from "./json.ts";
import type { JsonValue } from "./json.ts";
import { matches, versioned } from "./schema.ts";
import type { Contract } from "./schema.ts";

export { BUNDLE, counter, isServiceToWorker, isWorkerToService, MAX_CHUNK, MAX_ITEM_TEXT_BYTES, MAX_PATH_BYTES, MAX_TRANSFER, MAX_TRANSFERS, newId, parseId, record, VERSION } from "./schema.ts";

export { decodeWorkerFrame, validateWorker } from "./worker/contract.ts";

const client: Contract = { root: clientSchema, definitions: clientSchema.$defs };

export function validate(value: JsonValue): asserts value is Envelope {
  if (!matches(versioned(value), client.root, client)) throw new WireError("invalid_schema");
}

export function decode(bytes: Uint8Array): Envelope {
  const value = boundedJson(bytes);
  validate(value);

  return value;
}
