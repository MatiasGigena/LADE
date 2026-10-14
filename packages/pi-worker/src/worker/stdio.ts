import { Console } from "node:console";

const protocolWrite = process.stdout.write.bind(process.stdout);

globalThis.console = new Console({ stdout: process.stderr, stderr: process.stderr });

process.stdout.write = process.stderr.write.bind(process.stderr);

export function writeFrame(frame: Buffer): boolean {
  return protocolWrite(frame);
}
