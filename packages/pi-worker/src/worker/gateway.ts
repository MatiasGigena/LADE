import http from "node:http";
import type { ClientRequestArgs } from "node:http";
import https from "node:https";
import { connect, isIP } from "node:net";
import type { Socket } from "node:net";
import type { Duplex } from "node:stream";
import { connect as connectTls } from "node:tls";
import type { ConnectionOptions } from "node:tls";
import { Agent, buildConnector, install, setGlobalDispatcher } from "undici";

const MAX_CONNECT_RESPONSE = 16 * 1024;

const CONNECT_TIMEOUT_MS = 30_000;

const AUTHORITY = /^(?:\[[0-9A-Fa-f:.]+\]|[A-Za-z0-9.-]+):[0-9]{1,5}$/;

export class GatewayError extends Error {
  readonly code: string;
  constructor(code: string, message: string) { super(message); this.code = code; }
}

export function tunnel(socketPath: string, authority: string): Promise<Socket> {
  if (!AUTHORITY.test(authority)) return Promise.reject(new GatewayError("gateway_target", "invalid tunnel target"));

  return new Promise((resolve, reject) => {
    const socket = connect({ path: socketPath });
    let head = Buffer.alloc(0);

    function fail(error: Error): void {
      detach(); socket.destroy();
      reject(error);
    }

    function onTimeout(): void { fail(new GatewayError("gateway_timeout", "gateway CONNECT timed out")); }

    function onEnd(): void { fail(new GatewayError("gateway_protocol", "gateway closed the connection before replying")); }

    function detach(): void {
      socket.removeListener("data", onData); socket.removeListener("error", fail); socket.removeListener("end", onEnd);
      socket.removeListener("timeout", onTimeout); socket.setTimeout(0);
    }

    function onData(chunk: Buffer): void {
      head = Buffer.concat([head, chunk]);
      const end = head.indexOf("\r\n\r\n");

      if ((end < 0 ? head.length : end + 4) > MAX_CONNECT_RESPONSE) {
        fail(new GatewayError("gateway_protocol", "oversized CONNECT response"));

        return;
      }

      if (end < 0) {
        return;
      }

      socket.pause();
      detach();
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

    socket.setTimeout(CONNECT_TIMEOUT_MS);
    socket.once("timeout", onTimeout);
    socket.once("error", fail);
    socket.once("end", onEnd);
    socket.on("data", onData);
    socket.once("connect", () => {
      socket.write(`CONNECT ${authority} HTTP/1.1\r\nHost: ${authority}\r\n\r\n`);
    });
  });
}

type Connected = (error: Error | null, stream?: Duplex) => void;

function authorityOf(host: string, port: number | string): string {
  return host.includes(":") && !host.startsWith("[") ? `[${host}]:${port}` : `${host}:${port}`;
}

function hostOf(options: ClientRequestArgs): string {
  return options.hostname ?? options.host ?? "localhost";
}

class GatewayHttpAgent extends http.Agent {
  readonly socketPath: string;

  constructor(socketPath: string) {
    super({ keepAlive: true });
    this.socketPath = socketPath;
  }

  override createConnection(options: ClientRequestArgs, callback: Connected): undefined {
    tunnel(this.socketPath, authorityOf(hostOf(options), options.port ?? 80)).then(
      socket => { callback(null, socket); socket.resume(); },
      (error: Error) => callback(error),
    );
  }
}

class GatewayHttpsAgent extends https.Agent {
  readonly socketPath: string;

  constructor(socketPath: string) {
    super({ keepAlive: true });
    this.socketPath = socketPath;
  }

  override createConnection(options: https.RequestOptions, callback: Connected): undefined {
    const host = hostOf(options);
    const servername = options.servername ?? (isIP(host) === 0 ? host : undefined);

    tunnel(this.socketPath, authorityOf(host, options.port ?? 443)).then(socket => {
      const { host: _host, hostname: _hostname, port: _port, path: _path, checkServerIdentity: _check, ...request } = options;
      const tlsOptions: ConnectionOptions = { ...request, socket, host };

      if (servername !== undefined) tlsOptions.servername = servername;
      const secure = connectTls(tlsOptions);

      const failed = (error: Error): void => callback(error);

      secure.once("error", failed);
      secure.once("secureConnect", () => { secure.removeListener("error", failed); callback(null, secure); });
    }, (error: Error) => callback(error));
  }
}

class NoNetworkHttpAgent extends http.Agent {
  override createConnection(_options: ClientRequestArgs, callback: Connected): undefined {
    queueMicrotask(() => callback(new GatewayError("network_unavailable", "network disabled: no LADE gateway socket")));
  }
}

class NoNetworkHttpsAgent extends https.Agent {
  override createConnection(_options: https.RequestOptions, callback: Connected): undefined {
    queueMicrotask(() => callback(new GatewayError("network_unavailable", "network disabled: no LADE gateway socket")));
  }
}

export function installGateway(socketPath: string): void {
  const tls = buildConnector({ timeout: CONNECT_TIMEOUT_MS });

  const connector: buildConnector.connector = (options, callback) => {
    const port = options.port || (options.protocol === "https:" ? "443" : "80");

    tunnel(socketPath, authorityOf(options.hostname, port)).then(socket => {
      if (options.protocol === "https:") tls({ ...options, httpSocket: socket }, callback);
      else callback(null, socket);
    }, (error: Error) => callback(error, null));
  };

  setGlobalDispatcher(new Agent({ allowH2: false, connect: connector }));
  install();
  http.globalAgent = new GatewayHttpAgent(socketPath);
  https.globalAgent = new GatewayHttpsAgent(socketPath);
}

export function installNoNetwork(): void {
  const connector: buildConnector.connector = (_options, callback) => {
    callback(new GatewayError("network_unavailable", "network disabled: no LADE gateway socket"), null);
  };

  setGlobalDispatcher(new Agent({ connect: connector }));
  install();
  http.globalAgent = new NoNetworkHttpAgent();
  https.globalAgent = new NoNetworkHttpsAgent();
}
