import clientSchema from "../../../../contracts/v1/schema.json" with { type: "json" };
import type { Envelope } from "../../src/dto.ts";
import { boundedJson, WireError } from "../../src/json.ts";
import type { JsonValue } from "../../src/json.ts";
import { matches, versioned } from "../../src/schema.ts";
import type { Contract } from "../../src/schema.ts";

const client: Contract = { root: clientSchema, definitions: clientSchema.$defs };

export function validate(value: JsonValue): asserts value is Envelope {
  if (!matches(versioned(value), client.root, client)) throw new WireError("invalid_schema");
}

export function decode(bytes: Uint8Array): Envelope {
  const value = boundedJson(bytes);
  validate(value);

  return value;
}
