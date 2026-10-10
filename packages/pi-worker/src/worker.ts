// LADE pi worker: one process per running Objective, launched by laded inside bwrap as
// `node packages/pi-worker/src/worker.ts`. Speaks delivery-plan §4.2 JSONL on stdin/stdout.
// The stdio guard must stay the first import so SDK console output can never reach stdout.
import { writeFrame } from "./worker/stdio.ts";
import type { AgentSession } from "@earendil-works/pi-coding-agent";
import { WireError } from "./json.ts";
import type { WorkerErrorCode } from "./dto.ts";
import { ActivityMapper, safeMessage } from "./worker/activity.ts";
import { StdioChannel } from "./worker/channel.ts";
import { installGateway, installNoNetwork } from "./worker/gateway.ts";
import { abort, deliver, openSession, StartError } from "./worker/session.ts";

/** Exit statuses: clean shutdown/EOF, start failure, protocol violation, internal failure. */
export const EXIT_OK = 0;

export const EXIT_START = 1;

export const EXIT_PROTOCOL = 2;

export const EXIT_INTERNAL = 70;

// SDK startup paths (version checks, telemetry, package updates) stay offline in the worker.
process.env.PI_OFFLINE = "1";

process.env.PI_SKIP_VERSION_CHECK = "1";

process.env.PI_TELEMETRY = "0";

const gatewaySocket = process.env.LADE_GATEWAY_SOCKET;

if (gatewaySocket) installGateway(gatewaySocket);
else installNoNetwork();

const channel = new StdioChannel(process.stdin, writeFrame, process.stdout);

// A closed stdout means the supervising service is gone; nothing more can be reported.
process.stdout.on("error", () => process.exit(EXIT_INTERNAL));

function report(code: WorkerErrorCode, message: string): void {
  void channel.send({ kind: "worker.error", code, message: safeMessage(message) });
}

async function exit(status: number, session: AgentSession | undefined): Promise<void> {
  session?.dispose();
  await channel.flush();
  process.exit(status);
}

function failed(error: Error): void {
  // Diagnostics carry error class names only: provider/tool errors may quote prompt or source text.
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
  let session: AgentSession | undefined;
  let mapper: ActivityMapper | undefined;

  try {
    for await (const body of channel.incoming()) {
      if (session === undefined || mapper === undefined) {
        if (body.kind !== "worker.start") {
          report("protocol_violation", "the first frame must be worker.start");

          return exit(EXIT_PROTOCOL, session);
        }

        try {
          session = await openSession(body);
        } catch (error) {
          process.stderr.write(`lade-worker: start failed (${error instanceof Error ? error.name : "unknown"})\n`);
          report(error instanceof StartError ? error.code : "startup_failed", error instanceof StartError ? error.message : "could not open the pi session");

          return exit(EXIT_START, undefined);
        }

        const activity = new ActivityMapper(item => { void channel.send(item); });
        mapper = activity;
        session.subscribe(event => activity.handle(event));
        await channel.send({ kind: "worker.ready", pi_session_id: session.sessionId, session_file: session.sessionFile ?? "" });
        continue;
      }

      switch (body.kind) {
        case "worker.start":
          report("protocol_violation", "worker.start was already processed");

          return exit(EXIT_PROTOCOL, session);
        case "input.deliver":
          await channel.send({ kind: "input.received", input_id: body.input_id });

          try {
            await deliver(session, body, error => report("session_failed", error.message));
          } catch (error) {
            report("delivery_failed", error instanceof Error ? error.message : "input was rejected");
          }

          break;
        case "session.abort":
          await abort(session, cleared(mapper));

          break;

        case "worker.shutdown":
          if (session.isStreaming) await abort(session, cleared(mapper));

          return exit(EXIT_OK, session);
      }
    }

    // stdin EOF: the service closed the link; stop the run and leave cleanly.
    if (session?.isStreaming && mapper) await abort(session, cleared(mapper));

    return exit(EXIT_OK, session);
  } catch (error) {
    if (!(error instanceof WireError)) throw error;
    // Framing errors poison the link. The rejected bytes are never echoed.
    report("protocol_violation", `rejected inbound frame (${error.code})`);

    return exit(EXIT_PROTOCOL, session);
  }
}

await serve().catch(failed);
