import assert from "node:assert/strict";
import { corpus, run } from "./support.ts";
console.log(JSON.stringify(corpus.map(c => {
  const outcome = run(c); assert.equal(outcome, c.expected, c.id);
  return { id: c.id, outcome };
})));
