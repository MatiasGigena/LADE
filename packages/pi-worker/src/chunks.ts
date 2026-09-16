import { createHash } from "node:crypto";
import type { Body, Scope } from "./dto.ts";
import { WireError } from "./json.ts";
import { MAX_CHUNK, MAX_TRANSFER, MAX_TRANSFERS } from "./protocol.ts";

interface Transfer { total: number; digest: string; next: bigint; size: number; bytes: Buffer }

export class Reassembler {
  private readonly owner: Scope;
  private readonly generation: string;
  private readonly transfers = new Map<string, Transfer>();
  private failed = false;
  constructor(owner: Scope, generation: string) {
    this.owner = { ...owner }; this.generation = generation;
  }
  close(): void { this.transfers.clear(); this.failed = true; }
  accept(body: Body): Buffer | undefined {
    if (this.failed) throw new WireError("closed");
    try { return this.apply(body); }
    catch (error) { this.close(); throw error; }
  }
  private apply(body: Body): Buffer | undefined {
    switch (body.kind) {
      case "chunk.start": {
        if (Object.entries(this.owner).some(([key, v]) => body.owner[key as keyof Scope] !== v) || body.execution_generation !== this.generation) throw new WireError("stale_execution");
        if (this.transfers.has(body.transfer_id)) throw new WireError("chunk_order");
        const total = Number(body.total_bytes);
        if (!Number.isSafeInteger(total) || total <= 0 || total > MAX_TRANSFER || this.transfers.size === MAX_TRANSFERS) throw new WireError("limit_reached");
        this.transfers.set(body.transfer_id, { total, digest: body.sha256, next: 0n, size: 0, bytes: Buffer.alloc(0) });
        return;
      }
      case "chunk.data": {
        const t = this.transfers.get(body.transfer_id);
        if (!t || BigInt(body.index) !== t.next) throw new WireError("chunk_order");
        const n = body.hex.length / 2;
        if (!body.hex.length || body.hex.length % 2 || !/^[0-9a-f]+$/.test(body.hex) || n > MAX_CHUNK || t.size + n > t.total) throw new WireError("limit_reached");
        if (t.bytes.length < t.size + n) {
          const next = Buffer.alloc(Math.min(t.total, Math.max(t.size + n, t.bytes.length * 2)));
          t.bytes.copy(next, 0, 0, t.size); t.bytes = next;
        }
        t.bytes.write(body.hex, t.size, n, "hex");
        t.size += n; t.next++;
        return;
      }
      case "chunk.end": {
        const t = this.transfers.get(body.transfer_id);
        if (!t) throw new WireError("chunk_order");
        this.transfers.delete(body.transfer_id);
        if (t.size !== t.total) throw new WireError("chunk_length");
        if (createHash("sha256").update(t.bytes).digest("hex") !== t.digest) throw new WireError("chunk_checksum");
        return t.bytes;
      }
      case "chunk.cancel":
        if (!this.transfers.delete(body.transfer_id)) throw new WireError("chunk_order");
        return;
      default: throw new WireError("invalid_schema");
    }
  }
}
