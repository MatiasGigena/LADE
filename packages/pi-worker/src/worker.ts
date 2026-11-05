import { writeFrame } from "./worker/stdio.ts";
import { WireError } from "./json.ts";
import type { AgentSession } from "@earendil-works/pi-coding-agent";
import type { ServiceToWorker, WorkerErrorCode } from "./dto.ts";
import { ActivityMapper } from "./worker/activity.ts";
import { StdioChannel } from "./worker/channel.ts";
import { installGateway, installNoNetwork } from "./worker/gateway.ts";
import { abort, deliver, SessionHost, StartError } from "./worker/session.ts";
import type { InputGate } from "./worker/session.ts";
import { safeMessage } from "./worker/text.ts";
import { UiBridge } from "./worker/ui.ts";
import type { CommandBody, DeliverBody } from "./worker/wire.ts";

export const EXIT_OK = 0;

export const EXIT_START = 1;

export const EXIT_PROTOCOL = 2;

export const EXIT_INTERNAL = 70;

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

class Server {
  private readonly mapper = new ActivityMapper(item => { void channel.send(item); });
  private readonly ui = new UiBridge(body => { void channel.send(body); }, (title, text, isError) => this.mapper.system(title, text, isError));
  private host: SessionHost | undefined;
  private delivery = Promise.resolve();
  private preparing = 0;
  private inputEpoch = 0;

  async run(): Promise<void> {
    try {
      for await (const body of channel.incoming()) {
        const status = this.host === undefined ? await this.open(body) : await this.dispatch(this.host, body);

        if (status !== undefined) return exit(status, this.host);
      }

      await this.settle();

      return exit(EXIT_OK, this.host);
    } catch (error) {
      if (!(error instanceof WireError)) throw error;
      this.cancelInputs();
      report("protocol_violation", `rejected inbound frame (${error.code})`);

      return exit(EXIT_PROTOCOL, this.host);
    }
  }

  /** Handles the first frame, which must open the session; returns an exit status when the worker must stop. */
  private async open(body: ServiceToWorker): Promise<number | undefined> {
    if (body.kind !== "worker.start") {
      report("protocol_violation", "the first frame must be worker.start");

      return EXIT_PROTOCOL;
    }

    const opening = new SessionHost(body, item => { void channel.send(item); }, this.ui, session => session.subscribe(event => this.mapper.handle(event)), (title, text, isError) => this.mapper.system(title, text, isError));

    try {
      await opening.open();
    } catch (error) {
      process.stderr.write(`lade-worker: start failed (${error instanceof Error ? error.name : "unknown"})\n`);
      report(error instanceof StartError ? error.code : "startup_failed", error instanceof StartError ? error.message : "could not open the pi session");

      return EXIT_START;
    }

    this.host = opening;
    await channel.send({ kind: "worker.ready", pi_session_id: opening.session.sessionId, session_file: opening.session.sessionFile ?? body.session_dir });
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
        await this.enqueue(host, body);

        return undefined;
      case "session.abort":
        this.cancelInputs();
        host.session.abortCompaction();
        await abort(host.session, cleared(this.mapper));

        return undefined;
      case "session.command":
        await this.command(host, body);

        return undefined;
      case "ui.respond": {
        const refused = this.ui.respond(body);

        if (refused !== undefined) this.mapper.system("Dialog answer refused", refused, true);

        return undefined;
      }

      case "worker.shutdown":
        await this.settle();

        return EXIT_OK;
      default: {
        const unhandled: never = body;

        return unhandled;
      }
    }
  }

  private cancelInputs(): void {
    this.inputEpoch++;
    this.ui.cancel();
  }

  /** Cancels pending inputs and aborts a running turn before the worker exits. */
  private async settle(): Promise<void> {
    this.cancelInputs();

    if (this.host?.session.isStreaming) await abort(this.host.session, cleared(this.mapper));
  }

  private async enqueue(host: SessionHost, body: DeliverBody): Promise<void> {
    const session = host.session;
    await channel.send({ kind: "input.received", input_id: body.input_id });
    const epoch = this.inputEpoch;
    this.preparing++;
    this.delivery = this.delivery.then(() => this.deliverInput(session, host.gate, body, epoch));
    await this.ui.runUntilDialog(() => this.delivery);
  }

  private async deliverInput(session: AgentSession, gate: InputGate, body: DeliverBody, epoch: number): Promise<void> {
    const current = (): boolean => epoch === this.inputEpoch;

    try {
      if (!current()) return;
      await deliver(session, gate, body, error => report("session_failed", error.message), current);
    } catch (error) {
      if (current()) report("delivery_failed", error instanceof Error ? error.message : "input was rejected");
    } finally {
      this.preparing--;
    }
  }

  private async command(host: SessionHost, body: CommandBody): Promise<void> {
    if (this.preparing > 0) {
      await channel.send({ kind: "session.result", command_id: body.command_id, ok: false, message: "input delivery is in progress; answer its dialog, stop or wait for it to finish" });

      return;
    }

    void host.command(body).then(result => channel.send(result));
  }
}

await new Server().run().catch(failed);
