import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import http, { createServer } from "node:http";
import type { Server } from "node:http";
import { connect, createServer as createNetServer } from "node:net";
import type { AddressInfo, Socket } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { getGlobalDispatcher } from "undici";
import { installGateway, installNoNetwork, tunnel } from "../src/worker/gateway.ts";

const root = mkdtempSync(join(tmpdir(), "lade-gateway-"));

const socketPath = join(root, "gateway.sock");

const authorities: string[] = [];

const open = new Set<Socket>();

function isInfo(address: string | AddressInfo | null): address is AddressInfo {
  return address !== null && Object(address) === address;
}

function portOf(server: Server): number {
  const address = server.address();
  assert.ok(isInfo(address), "the origin listens on TCP");

  return address.port;
}

const origin = createServer((request, response) => {
  response.end(`origin saw ${request.url}`);
});

origin.on("upgrade", (request, socket: Socket) => {
  open.add(socket);
  socket.once("close", () => open.delete(socket));
  const accept = createHash("sha1").update(`${request.headers["sec-websocket-key"] ?? ""}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`).digest("base64");
  socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
  socket.once("data", (frame: Buffer) => {
    const length = (frame[1] ?? 0) & 0x7f;
    const mask = frame.subarray(2, 6);
    const payload = Buffer.from(frame.subarray(6, 6 + length).map((byte, index) => byte ^ (mask[index % 4] ?? 0)));
    socket.write(Buffer.concat([Buffer.from([0x81, payload.length]), payload]));
  });
});

const gateway = createNetServer(client => {
  open.add(client);
  client.once("close", () => open.delete(client));
  let head = "";

  function onData(chunk: Buffer): void {
    head += chunk.toString("latin1");
    const end = head.indexOf("\r\n\r\n");

    if (end < 0) return;
    client.removeListener("data", onData);
    const authority = /^CONNECT (\S+) HTTP\/1\.1\r\n/.exec(head)?.[1] ?? "";
    authorities.push(authority);

    if (authority.startsWith("refused.")) {
      client.end("HTTP/1.1 403 Forbidden\r\n\r\n");

      return;
    }

    if (authority.startsWith("silent.")) {
      client.end();

      return;
    }

    if (authority.startsWith("oversized.")) {
      client.end(`HTTP/1.1 200 Connection Established\r\nX-Pad: ${"x".repeat(17 * 1024)}\r\n\r\n`);

      return;
    }

    if (authority.startsWith("limit.") || authority.startsWith("over-limit.")) {
      const prefix = "HTTP/1.1 200 Connection Established\r\nX-Pad: ";
      const size = 16 * 1024 + (authority.startsWith("over-limit.") ? 1 : 0);
      client.end(`${prefix}${"x".repeat(size - Buffer.byteLength(prefix) - 4)}\r\n\r\n`);

      return;
    }

    if (authority.startsWith("payload.")) {
      client.end(`HTTP/1.1 200 Connection Established\r\n\r\n${"x".repeat(20 * 1024)}`);

      return;
    }

    const upstream = connect({ host: "127.0.0.1", port: portOf(origin) }, () => {
      client.write("HTTP/1.1 200 Connection Established\r\n\r\n");
      client.pipe(upstream); upstream.pipe(client);
    });

    open.add(upstream);
    upstream.once("close", () => open.delete(upstream));
  }

  client.on("data", onData);
});

await new Promise<void>(resolve => origin.listen(0, "127.0.0.1", resolve));

await new Promise<void>(resolve => gateway.listen(socketPath, resolve));

after(async () => {
  await getGlobalDispatcher().destroy();

  for (const socket of open) socket.destroy();
  origin.closeAllConnections();
  await new Promise(resolve => origin.close(resolve));
  await new Promise(resolve => gateway.close(resolve));
  rmSync(root, { recursive: true, force: true });
});

test("fetch is tunnelled through the gateway socket with HTTP CONNECT", async () => {
  installGateway(socketPath);
  const port = portOf(origin);
  const response = await fetch(`http://provider.lade.test:${port}/v1/models`);
  assert.equal(await response.text(), "origin saw /v1/models");
  assert.deepEqual(authorities, [`provider.lade.test:${port}`]);
});

function httpGet(url: string): Promise<string> {
  return new Promise((resolve, reject) => {
    http.get(url, response => {
      let body = "";

      response.setEncoding("utf8");
      response.on("data", (chunk: string) => { body += chunk; });
      response.once("end", () => resolve(body));
    }).once("error", reject);
  });
}

test("node:http clients use the global agent, which tunnels through the gateway", async () => {
  installGateway(socketPath);
  const port = portOf(origin);
  assert.equal(await httpGet(`http://sdk.lade.test:${port}/v1/chat`), "origin saw /v1/chat");
  assert.equal(authorities.at(-1), `sdk.lade.test:${port}`);
  await assert.rejects(httpGet("http://refused.lade.test:80/"), { code: "gateway_refused" });
  http.globalAgent.destroy();
});

function echo(url: string): Promise<string> {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url);

    socket.addEventListener("open", () => socket.send("ping"));
    socket.addEventListener("message", event => { resolve(String(event.data)); socket.close(); });
    socket.addEventListener("error", () => reject(new Error("websocket failed")));
  });
}

test("WebSocket connections are tunnelled through the gateway", async () => {
  installGateway(socketPath);
  const port = portOf(origin);
  assert.equal(await echo(`ws://realtime.lade.test:${port}/v1/responses`), "ping");
  assert.equal(authorities.at(-1), `realtime.lade.test:${port}`);
  await assert.rejects(echo("ws://refused.lade.test:80/"), /websocket failed/);
  assert.equal(authorities.at(-1), "refused.lade.test:80");
});

test("a refused tunnel fails the request", async () => {
  await assert.rejects(tunnel(socketPath, "refused.lade.test:443"), { code: "gateway_refused" });
  await assert.rejects(tunnel(socketPath, "bad host:443"), { code: "gateway_target" });
});

test("a complete oversized CONNECT header is refused", async () => {
  await assert.rejects(tunnel(socketPath, "oversized.lade.test:443").then(socket => {
    socket.destroy();

    return socket;
  }), { code: "gateway_protocol" });
  const exact = await tunnel(socketPath, "limit.lade.test:443");
  exact.destroy();
  await assert.rejects(tunnel(socketPath, "over-limit.lade.test:443").then(socket => {
    socket.destroy();

    return socket;
  }), { code: "gateway_protocol" });
});

test("CONNECT preserves tunneled payload arriving with the response header", async () => {
  const socket = await tunnel(socketPath, "payload.lade.test:443");
  const chunks: Buffer[] = [];

  for await (const chunk of socket) chunks.push(chunk);
  const payload = Buffer.concat(chunks);
  assert.equal(payload.length, 20 * 1024);
  assert.equal(payload.equals(Buffer.alloc(20 * 1024, "x")), true);
});

test("without a gateway socket every connection fails with a clear reason", async () => {
  installNoNetwork();
  await assert.rejects(httpGet("http://provider.lade.test/"), { code: "network_unavailable" });
  await assert.rejects(fetch("http://provider.lade.test/"), (error: Error) => {
    assert.match(String(error.cause), /network disabled: no LADE gateway socket/);

    return true;
  });
  const tunnels = authorities.length;
  await assert.rejects(echo(`ws://provider.lade.test:${portOf(origin)}/`), /websocket failed/);
  assert.equal(authorities.length, tunnels);
});

test("a tunnel closed before the gateway replies fails instead of hanging", { timeout: 5_000 }, async () => {
  await assert.rejects(tunnel(socketPath, "silent.lade.test:443"), { code: "gateway_protocol" });
});

test("an established tunnel is not destroyed by its user's own idle timeout", { timeout: 5_000 }, async () => {
  const socket = await tunnel(socketPath, `idle.lade.test:${portOf(origin)}`);
  open.add(socket);
  socket.setTimeout(20);
  await new Promise(resolve => socket.once("timeout", resolve));
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(socket.destroyed, false);
  socket.destroy();
});
