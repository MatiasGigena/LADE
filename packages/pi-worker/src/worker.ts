// Must stay the first import: stdio.ts redirects console and stdout to stderr before every other module (above all the SDK)
// is evaluated, so that only protocol frames reach stdout.
import { writeFrame } from "./worker/stdio.ts";
import type { ServiceToWorker, WorkerErrorCode, WorkerToService } from "./dto.ts";
import { ActivityMapper } from "./worker/activity.ts";
import { InboundError, StdioChannel } from "./worker/channel.ts";
import { installGateway, installNoNetwork } from "./worker/gateway.ts";
import { SessionHost, StartError } from "./worker/session.ts";
import { UiBridge } from "./worker/ui.ts";
import { workerError } from "./worker/wire.ts";

const EXIT_OK = 0;

const EXIT_START = 1;

const EXIT_PROTOCOL = 2;

const EXIT_INTERNAL = 70;

const gatewaySocket = process.env.LADE_GATEWAY_SOCKET;

if (gatewaySocket) installGateway(gatewaySocket);
else installNoNetwork();

const channel = new StdioChannel(process.stdin, writeFrame, process.stdout);

process.stdout.on("error", () => process.exit(EXIT_INTERNAL));

/** Queues a frame. A frame the worker cannot encode is a worker bug: its rejection reaches `failed` as unhandled. */
function emit(body: WorkerToService): void {
  void channel.send(body);
}

function report(code: WorkerErrorCode, message: string): void {
  emit(workerError(code, message));
}

async function exit(status: number, host: SessionHost | undefined): Promise<void> {
  await host?.dispose().catch(() => undefined);
  await channel.flush();
  process.exit(status);
}

function failed(cause: unknown): void {
  process.stderr.write(`lade-worker: internal failure (${cause instanceof Error ? cause.name : "non-error"})\n`);
  report("internal", "the worker failed unexpectedly");
  void exit(EXIT_INTERNAL, undefined);
}

process.on("uncaughtException", failed);

process.on("unhandledRejection", failed);

/** Routes service frames: the first opens the session, the rest go to its `SessionHost`. */
class Server {
  private readonly mapper = new ActivityMapper(emit);
  private readonly ui = new UiBridge(emit, this.mapper.system);
  private host: SessionHost | undefined;

  async run(): Promise<void> {
    try {
      for await (const body of channel.incoming()) {
        const status = this.host === undefined ? await this.open(body) : await this.dispatch(this.host, body);

        if (status !== undefined) return exit(status, this.host);
      }

      await this.host?.abort();

      return exit(EXIT_OK, this.host);
    } catch (error) {
      if (!(error instanceof InboundError)) throw error;
      this.host?.cancelInputs();
      report("protocol_violation", error.message);

      return exit(EXIT_PROTOCOL, this.host);
    }
  }

  /** Handles the first frame, which must open the session; returns an exit status when the worker must stop. */
  private async open(body: ServiceToWorker): Promise<number | undefined> {
    if (body.kind !== "worker.start") {
      report("protocol_violation", "the first frame must be worker.start");

      return EXIT_PROTOCOL;
    }

    const opening = new SessionHost(body, emit, this.ui, this.mapper);

    try {
      await opening.open();
    } catch (error) {
      process.stderr.write(`lade-worker: start failed (${error instanceof Error ? error.name : "unknown"})\n`);
      report(error instanceof StartError ? error.code : "startup_failed", error instanceof StartError ? error.message : "could not open the pi session");

      return EXIT_START;
    }

    this.host = opening;
    opening.announce();

    return undefined;
  }

  /** Handles a frame of an open session; returns an exit status when the worker must stop. */
  private async dispatch(host: SessionHost, body: ServiceToWorker): Promise<number | undefined> {
    switch (body.kind) {
      case "worker.start":
        report("protocol_violation", "worker.start was already processed");

        return EXIT_PROTOCOL;
      case "input.deliver":
        await host.enqueue(body);

        return undefined;
      case "session.abort":
        await host.abort();

        return undefined;
      case "session.command":
        // Not awaited: a command can wait for a dialog that a later frame answers.
        void host.command(body).then(emit);

        return undefined;
      case "ui.respond": {
        const refused = this.ui.respond(body);

        if (refused !== undefined) this.mapper.system("Dialog answer refused", refused, true);

        return undefined;
      }

      case "worker.shutdown":
        await host.abort();

        return EXIT_OK;
      default: {
        const unhandled: never = body;

        return unhandled;
      }
    }
  }
}

await new Server().run().catch(failed);
