# Pinned narrow pi product-fork record

Initial compatibility baseline from decision #6: pi **0.85.1**. Current pin, a manually
approved upgrade: pi **1.5.0**, commit `f9a26365568127461c47a07a37dcb42c140a3ac5` (tag
`v1.5.0`), Node **22.23.1**. `manifest.json` records the source archive and byte hashes of
the unmodified upstream files retained here: the coding-agent package manifest, the release
install lock (`upstream/install-lock/`, upstream's `packages/coding-agent/install-lock/`) and
the MIT notice. No historical prototype is imported. **Patch inventory: empty.** No
demonstrated SDK gap requires an upstream patch.

This is the fork's pinned provenance/patch record, not a built or conformance-tested SDK
distribution. From 1.0 pi publishes no `npm-shrinkwrap.json`: the package manifest pins its
direct dependencies exactly, and the install lock (the lockfile root upstream's installer and
updater use) pins the transitive tree; together with the commit they are authoritative, never
a floating npm install. The lockstep pi packages in that lock have their registry URL but no
integrity entry, because upstream generates the lock from its monorepo workspace before they are
published (`scripts/generate-coding-agent-install-lock.mjs`); the source commit/archive remains
necessary. IMP-008 must materialize the narrow coding-agent fork, build/package it
reproducibly and run its adapter seams under Rust supervision. Do not substitute the
harness's globally installed pi.

Each future patch records path, SHA-256, rationale, regression command and upgrade
reconsideration. Keep agent-core/provider packages unchanged without demonstrated
need. Upgrades are manual coordinated changes, never independent pi self-updates.
No profile/credential import, provider calls, resource loading or package scripts
were executed to retain these files. Full shipped native/transitive notices belong
to IMP-014; this MIT notice is not blanket licensing clearance for that bundle.

## npm distribution used by the worker (WP-D)

`packages/pi-worker` installs `@earendil-works/pi-coding-agent@1.5.0` from the npm
registry (integrity recorded in `manifest.json` → `npm_distribution`, and in
`packages/pi-worker/package-lock.json`). Its `package.json` is byte-identical to
`upstream/package.json`, and the lockfile resolves the SDK's whole dependency closure to
exactly `upstream/install-lock/package-lock.json` (names, versions, resolved URLs and
integrity); `packages/pi-worker/test/worker-deps.test.ts` checks both, and
`scripts/check-pins.py` checks the hashes and lockstep versions. With the empty patch
inventory this tarball is the fork's runnable form; a reproducible build from the source
archive is still open and needed before the first patch. The worker imports root exports only
and applies **no patches**. Its only other direct dependency is `undici@8.12.3` (gateway
dispatcher, the version the SDK pins): the `lade-fixture` provider's stream factory and the
thinking-level helper come from the single hoisted pi-ai copy the SDK resolves
(`packages/pi-worker/src/worker/pi-ai.ts`), so exactly one pi-ai is installed and loaded. See
[docs/implementation/worker.md](../../docs/implementation/worker.md#dependencies).
