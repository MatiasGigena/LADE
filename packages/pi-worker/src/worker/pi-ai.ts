// The one `@earendil-works/pi-ai@0.85.1` the worker loads: the copy the pinned SDK itself resolves.
// pi-coding-agent ships an npm-shrinkwrap.json, so npm installs its whole tree (pi-ai included)
// nested under it and never hoists or dedupes it; a bare `@earendil-works/pi-ai` from the worker
// would need a second, separately locked registry copy. This path is pi-ai's own "." export target
// inside that nested install (layout pinned by package-lock.json and the upstream shrinkwrap);
// test/worker-deps.test.ts proves it is the instance pi-coding-agent and pi-agent-core load.
export { createAssistantMessageEventStream, getSupportedThinkingLevels } from "../../node_modules/@earendil-works/pi-coding-agent/node_modules/@earendil-works/pi-ai/dist/index.js";

export type { Api, AssistantMessage, AssistantMessageEventStream, Context, Model, SimpleStreamOptions, TextContent, ToolCall } from "../../node_modules/@earendil-works/pi-coding-agent/node_modules/@earendil-works/pi-ai/dist/index.js";
