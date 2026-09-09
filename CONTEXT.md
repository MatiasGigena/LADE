# LADE

LADE is a graphical Linux desktop environment for agentic software development, where agents perform development work under human direction and governance.

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
LADE’s TODE-powered code-editing surface, opened when inspection or direct editing is useful rather than serving as the product’s permanent center.
_Avoid_: Main interface, IDE shell

**Objective**:
A developer-defined outcome that LADE helps pursue and whose activity and result can be reviewed as a coherent unit.
_Avoid_: Prompt, ticket, chat

**Agent Session**:
A persistent pi interaction context through which work proceeds on an Objective. It includes conversation and tool activity but is not itself the resulting work.
_Avoid_: Chat, agent

**Change Set**:
The reviewable repository changes produced for an Objective, presented together with their intent and verification evidence.
_Avoid_: Agent output, response, patch
