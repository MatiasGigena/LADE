import { createHash } from "node:crypto";
import type { Body, Id, Reply } from "../../src/dto.ts";
import { WireError } from "../../src/json.ts";
import type { JsonValue } from "../../src/json.ts";
import { record } from "./protocol.ts";

export type ReplyResult = Reply["result"];

export class RequestHistory {
  private readonly requests = new Map<Id, { operation: Id; digest: string }>();
  private readonly operations = new Map<Id, { digest: string; outcome: ReplyResult }>();
  observe(body: Body, fixtureOutcome: ReplyResult): ReplyResult {
    if (body.kind !== "request" || !("operation_id" in body)) throw new WireError("invalid_schema");
    const { request_id, ...content } = body;
    const operationId = body.operation_id;

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

    if (request && (request.operation !== operationId || request.digest !== digest)) throw new WireError("duplicate_conflict");
    const operation = this.operations.get(operationId);

    if (operation && operation.digest !== digest) throw new WireError("duplicate_conflict");

    if ((!request && this.requests.size === 256) || (!operation && this.operations.size === 256)) throw new WireError("limit_reached");
    const outcome = operation?.outcome ?? structuredClone(fixtureOutcome);
    this.requests.set(request_id, { operation: operationId, digest });
    this.operations.set(operationId, { digest, outcome });

    return structuredClone(outcome);
  }
}
