import { connect } from "node:net";
import type { Socket } from "node:net";
import { Agent, buildConnector, install, setGlobalDispatcher } from "undici";

const MAX_CONNECT_RESPONSE = 16 * 1024;

const CONNECT_TIMEOUT_MS = 30_000;

const AUTHORITY = /^(?:\[[0-9A-Fa-f:.]+\]|[A-Za-z0-9.-]+):[0-9]{1,5}$/;

export class GatewayError extends Error {
  readonly code: string;
  constructor(code: string, message: string) { super(message); this.code = code; }
}

/** Open `socketPath`, send `CONNECT authority`, and resolve with the tunnelled stream after a 2xx reply. */
export function tunnel(socketPath: string, authority: string): Promise<Socket> {
  if (!AUTHORITY.test(authority)) return Promise.reject(new GatewayError("gateway_target", "invalid tunnel target"));

  return new Promise((resolve, reject) => {
    const socket = connect({ path: socketPath });
    let head = Buffer.alloc(0);

    function fail(error: Error): void {
      socket.removeListener("data", onData); socket.destroy();
      reject(error);
    }

    function onData(chunk: Buffer): void {
      head = Buffer.concat([head, chunk]);
      const end = head.indexOf("\r\n\r\n");

      if (end < 0) {
        if (head.length > MAX_CONNECT_RESPONSE) fail(new GatewayError("gateway_protocol", "oversized CONNECT response"));

        return;
      }

      socket.removeListener("data", onData); socket.removeListener("error", fail); socket.setTimeout(0);
      const status = /^HTTP\/1\.[01] (\d{3})/.exec(head.subarray(0, end).toString("latin1"))?.[1];

      if (status === undefined || !status.startsWith("2")) {
        socket.destroy();
        reject(new GatewayError("gateway_refused", `gateway refused tunnel (status ${status ?? "invalid"})`));

        return;
      }

      const rest = head.subarray(end + 4);

      if (rest.length > 0) socket.unshift(rest);
      resolve(socket);
    }

    socket.setTimeout(CONNECT_TIMEOUT_MS, () => fail(new GatewayError("gateway_timeout", "gateway CONNECT timed out")));
    socket.once("error", fail);
    socket.on("data", onData);
    socket.once("connect", () => {
      socket.write(`CONNECT ${authority} HTTP/1.1\r\nHost: ${authority}\r\n\r\n`);
    });
  });
}

/**
 * Route every undici connection (and therefore global fetch) through the LADE gateway. TLS is
 * negotiated end-to-end with the origin over the tunnel; the gateway only sees the authority.
 */
export function installGateway(socketPath: string): void {
  const tls = buildConnector({ timeout: CONNECT_TIMEOUT_MS });

  const connector: buildConnector.connector = (options, callback) => {
    const port = options.port || (options.protocol === "https:" ? "443" : "80");

    tunnel(socketPath, `${options.hostname}:${port}`).then(socket => {
      if (options.protocol === "https:") tls({ ...options, httpSocket: socket }, callback);
      else callback(null, socket);
    }, (error: Error) => callback(error, null));
  };

  setGlobalDispatcher(new Agent({ allowH2: false, connect: connector }));
  // Node's bundled fetch and npm undici keep separate implementations; use one for both.
  install();
}

/** Without a gateway socket the worker has no network: fail every connection with a clear reason. */
export function installNoNetwork(): void {
  const connector: buildConnector.connector = (_options, callback) => {
    callback(new GatewayError("network_unavailable", "network disabled: no LADE gateway socket"), null);
  };

  setGlobalDispatcher(new Agent({ connect: connector }));
  install();
}
