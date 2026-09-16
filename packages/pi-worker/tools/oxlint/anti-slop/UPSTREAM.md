# Anti-slop provenance

Source repository: <https://github.com/dmmulroy/anti-slop>

Source revision: `c44ef22ca116d0ba62a3ff663a0bd13a3f3fa40b`

Pristine source path: `skills/install-anti-slop/assets/anti-slop/` at the revision above.

Installed paths:

- `packages/pi-worker/tools/oxlint/anti-slop/index.ts`
- `packages/pi-worker/tools/oxlint/anti-slop/effect/`
- `packages/pi-worker/tools/oxlint/anti-slop/rules/`
- `packages/pi-worker/tools/oxlint/anti-slop/shared/`
- `packages/pi-worker/tools/oxlint/anti-slop/vendor/`

Intentional deviations:

- The pristine plugin source is unchanged.
- The plugin is installed under the only JavaScript package so its imports resolve against that package's pinned development dependencies.
- The optional Effect plugin is retained as vendored source but is not registered because `@lade/pi-worker` has no direct `effect` dependency.
