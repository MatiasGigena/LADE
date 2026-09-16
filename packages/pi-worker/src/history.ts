import { createHash } from "node:crypto";
import type { Body, Id, Result } from "./dto.ts";
import { WireError } from "./json.ts";
import type { JsonValue } from "./json.ts";
import { record } from "./protocol.ts";

/** Bounded fixture correlation; not a durable acceptance journal or permission. */
export class RequestHistory {
  private readonly requests = new Map<Id, { operation: Id; digest: string }>();
  private readonly operations = new Map<Id, { digest: string; outcome: Result }>();
  observe(body: Body, fixtureOutcome: Result): Result {
    if (body.kind !== "command") throw new WireError("invalid_schema");
    const { request_id, ...content } = body;

    function canonical(value: JsonValue): JsonValue {
      if (Array.isArray(value)) return value.map(canonical);

      if (record(value)) {
        return Object.fromEntries(Object.keys(value).sort().map(key => {
          const entry = value[key];

          if (entry === undefined) throw new WireError("invalid_schema");

          return [key, canonical(entry)];
        }));
      }

      return value;
    }

    const digest = createHash("sha256").update(JSON.stringify(canonical(content))).digest("hex");
    const request = this.requests.get(request_id);

    if (request && (request.operation !== body.operation_id || request.digest !== digest)) throw new WireError("duplicate_conflict");
    const operation = this.operations.get(body.operation_id);

    if (operation && operation.digest !== digest) throw new WireError("duplicate_conflict");

    if ((!request && this.requests.size === 256) || (!operation && this.operations.size === 256)) throw new WireError("limit_reached");
    const outcome = operation?.outcome ?? structuredClone(fixtureOutcome);
    this.requests.set(request_id, { operation: body.operation_id, digest });
    this.operations.set(body.operation_id, { digest, outcome });

    return structuredClone(outcome);
  }
}
