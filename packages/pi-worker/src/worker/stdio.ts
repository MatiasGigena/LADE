// Imported first by ../worker.ts so it runs before the SDK modules evaluate: stdout carries only
// protocol frames. Every console method and every direct process.stdout.write go to stderr.
import { Console } from "node:console";

const protocolWrite = process.stdout.write.bind(process.stdout);

globalThis.console = new Console({ stdout: process.stderr, stderr: process.stderr });

process.stdout.write = process.stderr.write.bind(process.stderr);

/** The only writer of fd 1. Returns false when the caller must wait for "drain". */
export function writeFrame(frame: Buffer): boolean {
  return protocolWrite(frame);
}
