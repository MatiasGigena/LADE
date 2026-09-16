# Product vision

## Thesis and problem

**Delegate implementation without delegating understanding or acceptance of the system.**

LADE connects the human, the agent, and the resulting work. Generating a plausible explanation or patch is not enough: the developer must understand the effect, direct a precise correction, and decide whether the result belongs in the system. The primary collaboration unit is the **Objective and its resulting work**, not an isolated conversation.

LADE is neither chat with tools, an agent-count dashboard, nor a conventional IDE with a conversation attached. An Agent Session supports an Objective; it is not the Change Set. The [domain vocabulary](../../CONTEXT.md) remains authoritative.

## Initial user and boundary

One developer, one Linux machine, one Workspace centered on one Git repository, with concurrent Objectives in isolated worktrees. Local-first means no mandatory LADE account or service, not a promise that model providers work offline. The supported first-release environment remains Linux Mint 22.3 / Cinnamon / X11 / x86_64, per [#12](https://github.com/MatiasGigena/LADE/issues/12#issuecomment-5690312917).

Read-only Inspection and Contextual Feedback replace embedded editing in v1. No Integrated Editor or TODE dependency is required, and no external-editor integration is newly promised. This is not a permanent ban on future editing. The [accepted scope revision](https://github.com/MatiasGigena/LADE/issues/13#issuecomment-5691260255) supersedes editor-specific requirements, not agent/tool/terminal governance.

## Product principles

- **Authority:** understand and control what an agent may do. Show execution scope, permission requests, interruptions and pending operations; distinguish input, execution permission, review approval and integration. Rust is authoritative, not the client or a worker's claim.
- **Understanding:** investigate what changed, why it matters and how it relates to the code, independently of the agent's explanation. Offer complete files, search, expanded diff context, and access to related code and tests. These are not a promise of semantic navigation or IDE parity.
- **Intervention:** identify a precise correction, explicitly send version-bound Contextual Feedback, and compare the resulting revision. Agent assertions of resolution are not human confirmation.
- **Task-led surfaces:** discuss while defining the Objective, inspect while understanding results, compare revisions while checking a correction. Agent Session, tools and contextual terminal remain available without becoming mandatory centers of every task.
- **Continuity without hidden authority:** restore records and drafts honestly, display uncertainty, and never replay uncertain effects or restore expired approvals merely to make recovery look seamless.

Expected outcome, constraints and acceptance criteria must be visible without a long mandatory form. An agent may propose them; it cannot unilaterally redefine scope. Work can start as discussion before repository execution provisions the isolated worktree.

## What remains binding

The [readiness decision register](../planning/implementation-readiness.md#decision-register) links the accepted contracts rather than replacing them: narrow pinned pi fork and full core/provider/authentication compatibility; Rust System Layer / pi Intelligence Layer / presentation-only client; fail-closed host enforcement; protected Git lifecycle; durable acceptance and recovery; supported installation/update contract; keyboard/accessibility/attention behavior. A small first increment is not permission to shrink the eventual release contract silently.

## Success and evaluation

Success is a developer accepting a correct result with sufficient understanding and retained authority. Proposed evaluation against the developer's current workflow:

1. Use comparable repository tasks, starting state and acceptance criteria; record tool/model versions and task difficulty. Counterbalance task order where feasible to reduce learning effects.
2. Measure time until a **correct** result is accepted, time to reconstruct context after an interruption, correction iterations, and the developer's explanation of the change, risks and verification limits.
3. Include a deliberately seeded semantic error that can survive superficial tests. Observe whether the developer finds it through independent inspection, points to the responsible code, requests a correction and verifies the revision. Label the fixture and reveal the seed during debrief; never ship it as real work.
4. Record false acceptance, unresolved confusion and recovery cost, not just speed. Establish a baseline before setting improvement targets; no numerical success target is approved yet.

This exercise is a **proposal**, not observed product evidence. A polished image, positive prototype preference, coverage percentage, number of agents or generated lines cannot establish success. Coverage is not a passing test; fixture evidence is not a real run.

## Non-goals for v1

- Embedded editing, TODE packaging or editor-profile recovery; external-editor integration commitments.
- Universal code review, perfect semantic re-anchoring, IDE parity or advanced semantic navigation by default.
- Remote execution, multi-repository Workspaces, real-time multi-user collaboration, mandatory cloud services or accounts.
- Redesigning pi's agent loop, reducing provider/authentication support, an unsandboxed shortcut, or additional supported platforms.
- Graphical package management and ports of particular personal extensions (deferred by #6); public licensing/contribution governance during private solo development.

## Reading order and status

1. [First-release workflow](first-release-workflow.md): collaboration contracts and failure paths.
2. [UI principles](ui-principles.md): approved composition, corrections and missing reference attachments.
3. [Implementation readiness](../planning/implementation-readiness.md): evidence, unresolved choices, acceptance matrix and gated increments.

[#1](https://github.com/MatiasGigena/LADE/issues/1) remains the canonical decision map. This consolidation records the developer's approved product/visual direction; it does not approve a toolkit, measured budgets, technical prototype validity, or production implementation. New technical choices are explicitly marked as proposals in the linked documents.
