import assert from "node:assert/strict";
import { test } from "node:test";
import { boundedJson } from "../src/json.ts";
import { corpus, run } from "./support.ts";

for (const fixture of corpus) {
  test(fixture.id, () => assert.equal(run(fixture), fixture.expected));
}
test("Unicode decodes without normalization", () => {
  const text = "á\r\n\t漢字😀é\u2028\u2029";
  assert.equal(boundedJson(Buffer.from(JSON.stringify(text))), text);
});
