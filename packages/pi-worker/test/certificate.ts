import { generateKeyPairSync, sign } from "node:crypto";

// A self-signed P-256 certificate encoded by hand (DER), so TLS tests need no openssl binary.

const SEQUENCE = 0x30;

const SET = 0x31;

const INTEGER = 0x02;

const BIT_STRING = 0x03;

const OCTET_STRING = 0x04;

const OBJECT_ID = 0x06;

const UTF8_STRING = 0x0c;

const UTC_TIME = 0x17;

const DNS_NAME = 0x82;

const VERSION_TAG = 0xa0;

const EXTENSIONS_TAG = 0xa3;

const X509_V3 = 2;

const ECDSA_WITH_SHA256 = [0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];

const COMMON_NAME = [0x55, 0x04, 0x03];

const SUBJECT_ALT_NAME = [0x55, 0x1d, 0x11];

const HOUR_MS = 3_600_000;

const PEM_LINE = 64;

export interface Certificate { key: string; cert: string }

function der(tag: number, ...parts: Buffer[]): Buffer {
  const body = Buffer.concat(parts);
  const size = body.length;
  const length = size < 0x80 ? [size] : size < 0x100 ? [0x81, size] : [0x82, size >> 8, size & 0xff];

  return Buffer.concat([Buffer.from([tag, ...length]), body]);
}

function utcTime(at: number): Buffer {
  return der(UTC_TIME, Buffer.from(`${new Date(at).toISOString().replace(/[-:T]/g, "").slice(2, 14)}Z`));
}

/** A key and certificate (PEM) for `host`, valid from an hour ago for one day, named by its subjectAltName. */
export function selfSigned(host: string): Certificate {
  const { privateKey, publicKey } = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  const algorithm = der(SEQUENCE, der(OBJECT_ID, Buffer.from(ECDSA_WITH_SHA256)));
  const name = der(SEQUENCE, der(SET, der(SEQUENCE, der(OBJECT_ID, Buffer.from(COMMON_NAME)), der(UTF8_STRING, Buffer.from(host)))));
  const now = Date.now();
  const validity = der(SEQUENCE, utcTime(now - HOUR_MS), utcTime(now + 24 * HOUR_MS));
  const altName = der(SEQUENCE, der(OBJECT_ID, Buffer.from(SUBJECT_ALT_NAME)), der(OCTET_STRING, der(SEQUENCE, der(DNS_NAME, Buffer.from(host)))));
  const tbs = der(SEQUENCE, der(VERSION_TAG, der(INTEGER, Buffer.from([X509_V3]))), der(INTEGER, Buffer.from([1])), algorithm, name, validity, name, publicKey.export({ type: "spki", format: "der" }), der(EXTENSIONS_TAG, der(SEQUENCE, altName)));
  const certificate = der(SEQUENCE, tbs, algorithm, der(BIT_STRING, Buffer.from([0]), sign("sha256", tbs, privateKey)));
  const lines = certificate.toString("base64").match(new RegExp(`.{1,${PEM_LINE}}`, "g")) ?? [];

  return { key: privateKey.export({ type: "pkcs8", format: "pem" }).toString(), cert: `-----BEGIN CERTIFICATE-----\n${lines.join("\n")}\n-----END CERTIFICATE-----\n` };
}
