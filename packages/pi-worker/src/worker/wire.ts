// Hand-mirrored delivery-plan §4.2 worker wire shapes and a closed validator.
// This is the single swap point for WP-A's generated worker DTOs/validators:
// replace the types, parseInbound and encodeOutbound here and nothing else changes.
import { boundedJson, MAX_FRAME, MAX_STRING, WireError } from "../json.ts";
import type { JsonObject, JsonValue } from "../json.ts";
import type { WorkerErrorCode } from "../dto.ts";

export const WORKER_PROTOCOL = 1;

export const WORKER_BUNDLE = "0.2.0";

export type DeliveryMode = "prompt" | "steer" | "follow_up";

export type ActivityRole = "user" | "assistant" | "thinking" | "tool" | "system";

export type TurnState = "running" | "idle";

export interface ModelRef { provider: string; id: string }

export interface StartBody {
  kind: "worker.start";
  objective_id: string;
  session_dir: string;
  cwd: string;
  model: ModelRef;
  resume: boolean;
  system_note: string;
}

export interface DeliverBody { kind: "input.deliver"; input_id: string; mode: DeliveryMode; text: string }

export type InboundBody = StartBody | DeliverBody | { kind: "session.abort" } | { kind: "worker.shutdown" };

export interface ActivityItemBody {
  kind: "activity.item";
  item_id: string;
  role: ActivityRole;
  title: string;
  text: string;
  tool_name?: string;
  is_error: boolean;
  truncated: boolean;
}

export interface ActivityDeltaBody { kind: "activity.delta"; item_id: string; append_text: string }

export type OutboundBody =
  | { kind: "worker.ready"; pi_session_id: string; session_file: string }
  | { kind: "input.received"; input_id: string }
  | ActivityItemBody
  | ActivityDeltaBody
  | { kind: "turn.state"; state: TurnState }
  | { kind: "worker.error"; code: WorkerErrorCode; message: string };

/** Transport seam: swapping to WP-A's validators must not change worker logic. */
export interface WorkerChannel {
  send(body: OutboundBody): Promise<void>;
  incoming(): AsyncIterable<InboundBody>;
}

export const MAX_ERROR_MESSAGE = 512;

const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

const ERROR_CODES: ReadonlySet<string> = new Set(["startup_failed", "session_failed", "provider_failed", "delivery_failed", "protocol_violation", "limit_reached", "internal"]);

function isObject(value: JsonValue | undefined): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isText(value: JsonValue | undefined): value is string {
  return typeof value === "string";
}

function isFlag(value: JsonValue | undefined): value is boolean {
  return typeof value === "boolean";
}

function isUuid(value: JsonValue | undefined): value is string {
  return isText(value) && UUID_V4.test(value);
}

function isMode(value: JsonValue | undefined): value is DeliveryMode {
  return value === "prompt" || value === "steer" || value === "follow_up";
}

function hasKeys(value: JsonObject, keys: readonly string[]): boolean {
  const own = Object.keys(value);

  return own.length === keys.length && keys.every(key => Object.hasOwn(value, key));
}

function invalid(): WireError {
  return new WireError("invalid_schema");
}

function parseBody(body: JsonObject): InboundBody {
  switch (body.kind) {
    case "worker.start": {
      const model = body.model;

      if (
        !hasKeys(body, ["kind", "objective_id", "session_dir", "cwd", "model", "resume", "system_note"]) ||
        !isUuid(body.objective_id) || !isText(body.session_dir) || !isText(body.cwd) ||
        !isFlag(body.resume) || !isText(body.system_note) ||
        !isObject(model) || !hasKeys(model, ["provider", "id"]) || !isText(model.provider) || !isText(model.id) ||
        body.session_dir.length === 0 || body.cwd.length === 0 || model.provider.length === 0 || model.id.length === 0
      ) throw invalid();

      return {
        kind: "worker.start",
        objective_id: body.objective_id,
        session_dir: body.session_dir,
        cwd: body.cwd,
        model: { provider: model.provider, id: model.id },
        resume: body.resume,
        system_note: body.system_note,
      };
    }

    case "input.deliver":
      if (!hasKeys(body, ["kind", "input_id", "mode", "text"]) || !isUuid(body.input_id) || !isMode(body.mode) || !isText(body.text)) throw invalid();

      return { kind: "input.deliver", input_id: body.input_id, mode: body.mode, text: body.text };
    case "session.abort":
      if (!hasKeys(body, ["kind"])) throw invalid();

      return { kind: "session.abort" };
    case "worker.shutdown":
      if (!hasKeys(body, ["kind"])) throw invalid();

      return { kind: "worker.shutdown" };
    default: throw invalid();
  }
}

/** Bounded parse (IMP-001 JSON limits) followed by the closed §4.2 inbound schema. */
export function parseInbound(bytes: Uint8Array): InboundBody {
  const value = boundedJson(bytes);

  if (!isObject(value) || value.protocol !== WORKER_PROTOCOL || value.bundle !== WORKER_BUNDLE) throw new WireError("version_mismatch");
  const body = value.body;

  if (!hasKeys(value, ["protocol", "bundle", "body"]) || !isObject(body)) throw invalid();

  return parseBody(body);
}

function checkText(value: string): void {
  if (Buffer.byteLength(value) > MAX_STRING) throw new WireError("limit_reached");
}

function checkOutbound(body: OutboundBody): void {
  switch (body.kind) {
    case "worker.ready": checkText(body.pi_session_id); checkText(body.session_file);

      return;
    case "input.received":
      if (!UUID_V4.test(body.input_id)) throw invalid();

      return;
    case "activity.item":
      if (!UUID_V4.test(body.item_id)) throw invalid();
      checkText(body.title); checkText(body.text);

      if (body.tool_name !== undefined) checkText(body.tool_name);

      return;
    case "activity.delta":
      if (!UUID_V4.test(body.item_id)) throw invalid();
      checkText(body.append_text);

      return;
    case "turn.state": return;
    case "worker.error":
      if (!ERROR_CODES.has(body.code) || body.message.length > MAX_ERROR_MESSAGE) throw invalid();
  }
}

/** One LF-terminated worker frame; rejects before writing anything over the frame limit. */
export function encodeOutbound(body: OutboundBody): Buffer {
  checkOutbound(body);
  const bytes = Buffer.from(JSON.stringify({ protocol: WORKER_PROTOCOL, bundle: WORKER_BUNDLE, body }));

  if (bytes.length > MAX_FRAME) throw new WireError("frame_size");

  return Buffer.concat([bytes, Buffer.from("\n")]);
}

/**
 * Worker LF framing with the IMP-001 semantics of `Decoder("worker")` in ../framing.ts: split only
 * on byte LF, bound the pending line before allocating, poison the decoder after the first error.
 * The shared Decoder validates against the IMP-001 client schema, so it cannot carry §4.2 bodies
 * until WP-A generalises it.
 */
export class LineDecoder {
  private body = Buffer.alloc(0);
  private size = 0;
  private failed = false;

  push(input: Buffer, emit: (frame: InboundBody) => void): void {
    if (this.failed) throw new WireError("closed");

    try {
      let pos = 0;

      while (pos < input.length) {
        const end = input.indexOf(10, pos);
        const stop = end < 0 ? input.length : end;
        const n = stop - pos;

        if (this.size + n > MAX_FRAME) throw new WireError("frame_size");

        if (this.body.length < this.size + n) {
          const next = Buffer.alloc(Math.min(MAX_FRAME, Math.max(this.size + n, this.body.length * 2, 1024)));
          this.body.copy(next, 0, 0, this.size); this.body = next;
        }

        input.copy(this.body, this.size, pos, stop);
        this.size += n; pos = stop;

        if (end >= 0) {
          const frame = parseInbound(this.body.subarray(0, this.size));
          this.size = 0; pos++; emit(frame);
        }
      }
    } catch (error) {
      this.failed = true; this.body = Buffer.alloc(0); this.size = 0;
      throw error;
    }
  }

  finish(): void {
    if (this.failed) throw new WireError("closed");
    this.failed = true;
    const partial = this.size !== 0;
    this.body = Buffer.alloc(0);

    if (partial) throw new WireError("truncated_frame");
  }
}
