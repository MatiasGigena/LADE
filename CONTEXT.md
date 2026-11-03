# LADE

LADE is a graphical Linux desktop environment for agentic software development: delegate implementation without delegating understanding or acceptance of the system. Its primary collaboration unit is the Objective and its resulting work, supported by Agent Sessions.

Its first release connects human authority, understanding and intervention through Read-only Inspection, Change Set review, and Contextual Feedback. It is local-first: one developer, one Linux machine, one Git-centered Workspace, with concurrent Objectives isolated in worktrees. It includes no embedded editor and makes no new commitment to external-editor integration; future editing is not permanently prohibited. See the [product vision](docs/product/vision.md) and [workflow contracts](docs/product/first-release-workflow.md).

The selected graphical client is **GTK4 native**, preserving the approved workbench design. Rust remains the authoritative System Layer and pi owns Agent Session behavior behind a pinned SDK adapter. See the [client architecture decision and remaining gates](docs/planning/client-architecture.md); toolkit selection does not authorize production implementation.

## Language

**Agentic Development Environment (ADE)**:
A development environment in which agents are first-class actors that can observe, plan, execute, and verify work while a human governs intent, permissions, and acceptance.
_Avoid_: AI IDE, IDE with chat, coding-assistant wrapper

**LADE**:
The graphical Linux desktop Agentic Development Environment defined by this project. Its primary interface is designed for human-agent collaboration rather than exposing a terminal application in a window.
_Avoid_: Lade, LADE IDE, terminal wrapper

**Workspace**:
A persistent LADE working environment centered on one Git repository. It may contain multiple agent sessions and isolated worktrees.
_Avoid_: Project, folder

**Integrated Editor**:
The TODE-powered code-editing surface explored in earlier planning. It is excluded from the first release; its previous integration policies remain historical, not first-release requirements.
_Avoid_: Read-only inspection, main interface, IDE shell

**Read-only Inspection**:
Viewing repository files and Change Sets to understand the work without modifying repository contents through the inspection surface.
_Avoid_: Integrated Editor, manual editing

**Contextual Feedback**:
Developer input tied to an Objective and a specific inspected version of code or changes, explicitly directed to an identified Agent Session to guide a revision. A draft is not submitted input; feedback is neither execution permission nor approval of the Change Set or authorization to integrate it.
_Avoid_: Integration approval, manual patch

**Objective**:
A developer-defined outcome that LADE helps pursue and whose activity and result can be reviewed as a coherent unit.
_Avoid_: Prompt, ticket, chat

**Agent Session**:
A persistent pi interaction context through which work proceeds on an Objective. It includes conversation and tool activity but is not itself the resulting work.
_Avoid_: Chat, agent

**Change Set**:
The reviewable net repository changes for an Objective relative to its Integration Target, presented together with their intent and verification evidence. They include any inherited changes not already present in that target.
_Avoid_: Agent output, response, patch

**Integration Target**:
The branch selected to receive an Objective’s accepted Change Set. It is distinct from the starting point from which the Objective’s work began.
_Avoid_: Starting branch, base (when meaning the destination)

**Cancellation**:
A reversible stop of an Objective that preserves its work and Agent Session for resumption.
_Avoid_: Discard, deletion

**Discard**:
The developer’s decision to abandon an Objective without integrating its Change Set. It is distinct from deleting the Objective’s execution resources.
_Avoid_: Cancellation, cleanup

**Cleanup**:
Explicit removal of a completed or discarded Objective’s branch and worktree while retaining the Objective’s record.
_Avoid_: Discard, Objective deletion
