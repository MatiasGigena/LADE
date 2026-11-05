## Agent skills

### Issue tracker

Issues and specs are tracked in GitHub Issues for `MatiasGigena/LADE`. See `docs/agents/issue-tracker.md`.

### Domain docs

This repository uses a single-context domain-doc layout. See `docs/agents/domain.md`.

## Code Quality

- Read files in full before wide-ranging changes, before editing files you have not fully inspected, and when asked to investigate or audit. Do not rely on search snippets for broad changes.
- No `any` unless absolutely necessary.
- Inline single-line helpers that have only one call site.
- Check `node_modules` for external API types; do not guess.
- No inline imports (`import("pkg").Type` or dynamic type imports). Use top-level imports only.
- Never remove or downgrade code to fix type errors from outdated dependencies; upgrade the dependency instead.
- Use only erasable TypeScript syntax (Node strip-only mode) in `packages/pi-worker/src` and `packages/pi-worker/test` (`erasableSyntaxOnly` in its `tsconfig.json`): no parameter properties, `enum`, `namespace`/`module`, `import =`, `export =`, or other constructs needing JavaScript emit. Use explicit fields with constructor assignments.
- Always ask before removing functionality or code that appears intentional.
- Do not preserve backward compatibility unless the user asks for it.
- Never hardcode key checks in the desktop client. Add defaults to `DEFAULT_APP_KEYBINDINGS` (`crates/lade-desktop/src/presentation/keys/mod.rs`) so they stay configurable.
