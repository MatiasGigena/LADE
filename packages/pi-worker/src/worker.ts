import { writeFrame } from "./worker/stdio.ts";
import { WireError } from "./json.ts";
import type { WorkerErrorCode } from "./dto.ts";
import { ActivityMapper, safeMessage } from "./worker/activity.ts";
import { StdioChannel } from "./worker/channel.ts";
import { installGateway, installNoNetwork } from "./worker/gateway.ts";
import { abort, deliver, SessionHost, StartError } from "./worker/session.ts";
import { UiBridge } from "./worker/ui.ts";

export const EXIT_OK = 0;

export const EXIT_START = 1;

export const EXIT_PROTOCOL = 2;

export const EXIT_INTERNAL = 70;

process.env.PI_OFFLINE = "1";

process.env.PI_SKIP_VERSION_CHECK = "1";

process.env.PI_TELEMETRY = "0";

const gatewaySocket = process.env.LADE_GATEWAY_SOCKET;

if (gatewaySocket) installGateway(gatewaySocket);
else installNoNetwork();

const channel = new StdioChannel(process.stdin, writeFrame, process.stdout);

process.stdout.on("error", () => process.exit(EXIT_INTERNAL));

function report(code: WorkerErrorCode, message: string): void {
  void channel.send({ kind: "worker.error", code, message: safeMessage(message) });
}

async function exit(status: number, host: SessionHost | undefined): Promise<void> {
  await host?.dispose().catch(() => undefined);
  await channel.flush();
  process.exit(status);
}

function failed(error: Error): void {
  process.stderr.write(`lade-worker: internal failure (${error.name})\n`);
  report("internal", "the worker failed unexpectedly");
  void exit(EXIT_INTERNAL, undefined);
}

process.on("uncaughtException", failed);

process.on("unhandledRejection", (reason: Error) => failed(reason instanceof Error ? reason : new Error("rejection")));

function cleared(mapper: ActivityMapper): (steering: number, followUp: number) => void {
  return (steering, followUp) => mapper.system("Queue cleared", `${steering} steering and ${followUp} follow-up message(s) were discarded before abort.`, false);
}

async function serve(): Promise<void> {
  let host: SessionHost | undefined;
  const mapper = new ActivityMapper(item => { void channel.send(item); });
  const ui = new UiBridge(body => { void channel.send(body); }, (title, text, isError) => mapper.system(title, text, isError));

  try {
    for await (const body of channel.incoming()) {
      if (host === undefined) {
        if (body.kind !== "worker.start") {
          report("protocol_violation", "the first frame must be worker.start");

          return exit(EXIT_PROTOCOL, host);
        }

        const opening = new SessionHost(body, item => { void channel.send(item); }, ui, session => session.subscribe(event => mapper.handle(event)), (title, text, isError) => mapper.system(title, text, isError));

        try {
          await opening.open();
        } catch (error) {
          process.stderr.write(`lade-worker: start failed (${error instanceof Error ? error.name : "unknown"})\n`);
          report(error instanceof StartError ? error.code : "startup_failed", error instanceof StartError ? error.message : "could not open the pi session");

          return exit(EXIT_START, undefined);
        }

        host = opening;
        await channel.send({ kind: "worker.ready", pi_session_id: host.session.sessionId, session_file: host.session.sessionFile ?? body.session_dir });
        host.announce();
        continue;
      }

      const session = host.session;

      switch (body.kind) {
        case "worker.start":
          report("protocol_violation", "worker.start was already processed");

          return exit(EXIT_PROTOCOL, host);
        case "input.deliver":
          await channel.send({ kind: "input.received", input_id: body.input_id });

          try {
            await deliver(session, body, error => report("session_failed", error.message));
          } catch (error) {
            report("delivery_failed", error instanceof Error ? error.message : "input was rejected");
          }

          break;
        case "session.abort":
          session.abortCompaction();
          await abort(session, cleared(mapper));

          break;
        case "session.command":
          void host.command(body).then(result => channel.send(result));

          break;
        case "ui.respond": {
          const refused = ui.respond(body);

          if (refused !== undefined) mapper.system("Dialog answer refused", refused, true);

          break;
        }

        case "worker.shutdown":
          if (session.isStreaming) await abort(session, cleared(mapper));

          return exit(EXIT_OK, host);
      }
    }

    if (host?.session.isStreaming) await abort(host.session, cleared(mapper));

    return exit(EXIT_OK, host);
  } catch (error) {
    if (!(error instanceof WireError)) throw error;
    report("protocol_violation", `rejected inbound frame (${error.code})`);

    return exit(EXIT_PROTOCOL, host);
  }
}

await serve().catch(failed);
