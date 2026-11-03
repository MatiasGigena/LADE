# Pinned narrow pi product-fork record

Initial compatibility baseline from decision #6: pi **0.85.1**, commit
`d981de1229ef899957bbe968bc8dcda02a21f477`, Node **22.23.1**.
`manifest.json` records the source archive and byte hashes of the unmodified
upstream package manifest, shrinkwrap and MIT notice retained here. No historical
prototype is imported. **Patch inventory: empty.** No demonstrated SDK gap in
IMP-001 requires an upstream patch, and no SDK code is loaded by this increment.

This is the fork's pinned provenance/patch record, not a built or conformance-tested
SDK distribution. The upstream package manifest contains ranges; its preserved
shrinkwrap and commit are authoritative, never a floating npm install. Some
lockstep packages in that shrinkwrap have no registry integrity because upstream
bundles them; the source commit/archive remains necessary. IMP-008 must materialize
the narrow coding-agent fork, build/package it reproducibly and run its adapter
seams under Rust supervision. Do not substitute the harness's globally installed pi.

Each future patch records path, SHA-256, rationale, regression command and upgrade
reconsideration. Keep agent-core/provider packages unchanged without demonstrated
need. Upgrades are manual coordinated changes, never independent pi self-updates.
No profile/credential import, provider calls, resource loading or package scripts
were executed to retain these files. Full shipped native/transitive notices belong
to IMP-014; this MIT notice is not blanket licensing clearance for that bundle.

## npm distribution used by the worker (WP-D)

`packages/pi-worker` installs `@earendil-works/pi-coding-agent@0.85.1` from the npm
registry (integrity recorded in `manifest.json` → `npm_distribution`, and in
`packages/pi-worker/package-lock.json`). Its `package.json` and `npm-shrinkwrap.json`
are byte-identical to `upstream/`, and the lockfile's nested tree under it is exactly
the upstream shrinkwrap; `packages/pi-worker/test/worker-deps.test.ts` checks all
three. With the empty patch inventory this tarball is the fork's runnable form; a
reproducible build from the source archive is still open and needed before the first
patch. The worker imports root exports only and applies **no patches**. Its only other
direct dependency is `undici@8.12.3` (gateway dispatcher): the `lade-fixture` provider's
stream factory and the thinking-level helper come from the pi-ai copy nested under the
SDK (`packages/pi-worker/src/worker/pi-ai.ts`), so exactly one pi-ai is installed and
loaded. See [docs/implementation/worker.md](../../docs/implementation/worker.md#dependencies).
