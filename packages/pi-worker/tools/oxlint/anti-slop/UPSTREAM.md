# Anti-slop provenance

Source repository: <https://github.com/dmmulroy/anti-slop>

Source revision: `13909c9e07590f5a85d93c62712259fe14a883f7`

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
