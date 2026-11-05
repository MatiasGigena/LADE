import http from "node:http";
import type { ClientRequestArgs } from "node:http";
import https from "node:https";
import { connect, isIP } from "node:net";
import type { Socket } from "node:net";
import type { Duplex } from "node:stream";
import { connect as connectTls } from "node:tls";
import type { ConnectionOptions } from "node:tls";
import { Agent, buildConnector, install, setGlobalDispatcher } from "undici";

export const MAX_CONNECT_RESPONSE = 16 * 1024;

const CONNECT_TIMEOUT_MS = 30_000;

const HTTP_PORT = 80;

const HTTPS_PORT = 443;

const HEADER_END = "\r\n\r\n";

const AUTHORITY = /^(?:\[[0-9A-Fa-f:.]+\]|[A-Za-z0-9.-]+):[0-9]{1,5}$/;

type GatewayCode = "network_unavailable" | "gateway_timeout" | "gateway_protocol" | "gateway_refused" | "gateway_target";

class GatewayError extends Error {
  readonly name = "GatewayError";
  readonly code: GatewayCode;
  constructor(code: GatewayCode, message: string) { super(message); this.code = code; }
}

/** A tunnel rejection as an Error; `tunnel` itself rejects only with `GatewayError`s and socket errors. */
function tunnelFailure(cause: unknown): Error {
  return cause instanceof Error ? cause : new GatewayError("gateway_protocol", "gateway tunnel failed");
}

function networkUnavailable(): GatewayError {
  return new GatewayError("network_unavailable", "network disabled: no LADE gateway socket");
}

/** Sends one CONNECT request on a gateway socket and settles once the gateway's response header is read. */
class ConnectHandshake {
  private readonly socket: Socket;
  private readonly authority: string;
  private readonly resolve: (socket: Socket) => void;
  private readonly reject: (error: Error) => void;
  private head = Buffer.alloc(0);

  constructor(socket: Socket, authority: string, resolve: (socket: Socket) => void, reject: (error: Error) => void) {
    this.socket = socket; this.authority = authority; this.resolve = resolve; this.reject = reject;
  }

  begin(): void {
    const socket = this.socket;
    socket.setTimeout(CONNECT_TIMEOUT_MS);
    socket.once("timeout", this.onTimeout);
    socket.once("error", this.fail);
    socket.once("end", this.onEnd);
    socket.on("data", this.onData);
    socket.once("connect", () => {
      socket.write(`CONNECT ${this.authority} HTTP/1.1\r\nHost: ${this.authority}${HEADER_END}`);
    });
  }

  private readonly fail = (error: Error): void => {
    this.detach(); this.socket.destroy();
    this.reject(error);
  };

  private readonly onTimeout = (): void => { this.fail(new GatewayError("gateway_timeout", "gateway CONNECT timed out")); };

  private readonly onEnd = (): void => { this.fail(new GatewayError("gateway_protocol", "gateway closed the connection before replying")); };

  private readonly onData = (chunk: Buffer): void => {
    this.head = Buffer.concat([this.head, chunk]);
    const end = this.head.indexOf(HEADER_END);

    if ((end < 0 ? this.head.length : end + HEADER_END.length) > MAX_CONNECT_RESPONSE) {
      this.fail(new GatewayError("gateway_protocol", "oversized CONNECT response"));

      return;
    }

    if (end >= 0) this.established(end);
  };

  private detach(): void {
    const socket = this.socket;
    socket.removeListener("data", this.onData); socket.removeListener("error", this.fail); socket.removeListener("end", this.onEnd);
    socket.removeListener("timeout", this.onTimeout); socket.setTimeout(0);
  }

  private established(end: number): void {
    const socket = this.socket;
    socket.pause();
    this.detach();
    const status = /^HTTP\/1\.[01] (\d{3})/.exec(this.head.subarray(0, end).toString("latin1"))?.[1];

    if (status === undefined || !status.startsWith("2")) {
      socket.destroy();
      this.reject(new GatewayError("gateway_refused", `gateway refused tunnel (status ${status ?? "invalid"})`));

      return;
    }

    const rest = this.head.subarray(end + HEADER_END.length);

    if (rest.length > 0) socket.unshift(rest);
    this.resolve(socket);
  }
}

export function tunnel(socketPath: string, authority: string): Promise<Socket> {
  if (!AUTHORITY.test(authority)) return Promise.reject(new GatewayError("gateway_target", "invalid tunnel target"));

  return new Promise((resolve, reject) => {
    new ConnectHandshake(connect({ path: socketPath }), authority, resolve, reject).begin();
  });
}

type Connected = (error: Error | null, stream?: Duplex) => void;

function authorityOf(host: string, port: number | string): string {
  return host.includes(":") && !host.startsWith("[") ? `[${host}]:${port}` : `${host}:${port}`;
}

function hostOf(options: ClientRequestArgs): string {
  return options.hostname ?? options.host ?? "localhost";
}

/** Runs TLS for an https request over an established tunnel. */
function secure(socket: Socket, options: https.RequestOptions, host: string, servername: string | undefined, callback: Connected): void {
  const { host: _host, hostname: _hostname, port: _port, path: _path, checkServerIdentity: _check, ...request } = options;
  const tlsOptions: ConnectionOptions = { ...request, socket, host };

  if (servername !== undefined) tlsOptions.servername = servername;
  const stream = connectTls(tlsOptions);

  const failed = (error: Error): void => callback(error);

  stream.once("error", failed);
  stream.once("secureConnect", () => { stream.removeListener("error", failed); callback(null, stream); });
}

class GatewayHttpAgent extends http.Agent {
  readonly socketPath: string;

  constructor(socketPath: string) {
    super({ keepAlive: true });
    this.socketPath = socketPath;
  }

  override createConnection(options: ClientRequestArgs, callback: Connected): undefined {
    tunnel(this.socketPath, authorityOf(hostOf(options), options.port ?? HTTP_PORT)).then(
      socket => { callback(null, socket); socket.resume(); },
      cause => callback(tunnelFailure(cause)),
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

    tunnel(this.socketPath, authorityOf(host, options.port ?? HTTPS_PORT)).then(
      socket => secure(socket, options, host, servername, callback),
      cause => callback(tunnelFailure(cause)),
    );
  }
}

class NoNetworkHttpAgent extends http.Agent {
  override createConnection(_options: ClientRequestArgs, callback: Connected): undefined {
    queueMicrotask(() => callback(networkUnavailable()));
  }
}

class NoNetworkHttpsAgent extends https.Agent {
  override createConnection(_options: https.RequestOptions, callback: Connected): undefined {
    queueMicrotask(() => callback(networkUnavailable()));
  }
}

type ConnectOptions = Parameters<buildConnector.connector>[0];

type ConnectCallback = Parameters<buildConnector.connector>[1];

/** Connects an undici request through a gateway tunnel, adding TLS for https origins. */
function connectThrough(socketPath: string, tls: buildConnector.connector, options: ConnectOptions, callback: ConnectCallback): void {
  const encrypted = options.protocol === "https:";

  tunnel(socketPath, authorityOf(options.hostname, options.port || (encrypted ? HTTPS_PORT : HTTP_PORT))).then(socket => {
    if (encrypted) tls({ ...options, httpSocket: socket }, callback);
    else callback(null, socket);
  }, cause => callback(tunnelFailure(cause), null));
}

export function installGateway(socketPath: string): void {
  const tls = buildConnector({ timeout: CONNECT_TIMEOUT_MS });

  setGlobalDispatcher(new Agent({ allowH2: false, connect: (options, callback) => connectThrough(socketPath, tls, options, callback) }));
  install();
  http.globalAgent = new GatewayHttpAgent(socketPath);
  https.globalAgent = new GatewayHttpsAgent(socketPath);
}

export function installNoNetwork(): void {
  const connector: buildConnector.connector = (_options, callback) => {
    callback(networkUnavailable(), null);
  };

  setGlobalDispatcher(new Agent({ connect: connector }));
  install();
  http.globalAgent = new NoNetworkHttpAgent();
  https.globalAgent = new NoNetworkHttpsAgent();
}
