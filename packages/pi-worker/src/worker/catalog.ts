import "./stdio.ts";
import { writeFileSync } from "node:fs";
import { getSupportedThinkingLevels } from "./pi-ai.ts";
import { ModelRuntime, VERSION } from "@earendil-works/pi-coding-agent";
import { registerTestFixture } from "./fixture.ts";
import { installNoNetwork } from "./gateway.ts";

interface CatalogModel { id: string; name: string; reasoning: boolean; thinking: string[] }

interface CatalogProvider { id: string; name: string; api_key: boolean; oauth: string | null; subscription: boolean; base_url: string | null; models: CatalogModel[] }

interface Catalog { sdk_version: string; providers: CatalogProvider[]; error: string | null }

installNoNetwork();

async function catalog(modelsPath: string | null, scratch: string): Promise<Catalog> {
  const runtime = await ModelRuntime.create({
    authPath: `${scratch}/auth.json`,
    modelsPath,
    modelsStorePath: `${scratch}/models-store.json`,
    allowModelNetwork: false,
    refreshOnCreate: false,
  });

  registerTestFixture(runtime);

  const providers = runtime.getProviders().map(provider => ({
    id: provider.id,
    name: provider.name,
    api_key: provider.auth.apiKey !== undefined,
    oauth: provider.auth.oauth?.name ?? null,
    subscription: provider.auth.oauth?.isSubscription === true,
    base_url: provider.baseUrl ?? null,
    models: runtime.getModels(provider.id).map(model => ({
      id: model.id,
      name: model.name,
      reasoning: model.reasoning,
      thinking: getSupportedThinkingLevels(model),
    })),
  }));

  return { sdk_version: VERSION, providers, error: runtime.getError() ?? null };
}

const output = process.argv[2];

if (output !== undefined) {
  const models = process.argv[3];
  const scratch = output.slice(0, Math.max(0, output.lastIndexOf("/"))) || ".";
  writeFileSync(output, JSON.stringify(await catalog(models ?? null, scratch)), { mode: 0o600 });
}
