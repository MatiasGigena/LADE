import assert from "node:assert/strict";
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

// Loopback-only fixtures: an origin HTTP server and a CONNECT gateway on a Unix socket.
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

/** GET through the `node:http` global agent. */
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

test("a refused tunnel fails the request", async () => {
  await assert.rejects(tunnel(socketPath, "refused.lade.test:443"), { code: "gateway_refused" });
  await assert.rejects(tunnel(socketPath, "bad host:443"), { code: "gateway_target" });
});

test("without a gateway socket every connection fails with a clear reason", async () => {
  installNoNetwork();
  await assert.rejects(httpGet("http://provider.lade.test/"), { code: "network_unavailable" });
  await assert.rejects(fetch("http://provider.lade.test/"), (error: Error) => {
    assert.match(String(error.cause), /network disabled: no LADE gateway socket/);

    return true;
  });
});
