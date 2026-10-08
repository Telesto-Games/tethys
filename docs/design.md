# Tethys — High-Level Design

> Status: **draft** · 2026-10-08

Tethys is a small, native, LLM-first IDE for Telesto Games' Unreal Engine projects.
It is **not** a general-purpose IDE. Everything it does starts from a `.uproject`.

## Principles

1. **Native and fast.** Written in Rust. No Electron, no webview, no JS UI layer. Use the GPU
   for rendering, start up in well under a second, and stay idle when nothing is happening.
2. **MVP first.** Each milestone ships the smallest thing that is useful. We add complexity
   only after the previous step has been used for real work.
3. **Unreal only.** The `.uproject` is the unit of work. We don't build features that only
   matter for generic codebases.
4. **The agent is the interface.** The main pane is a chat session with a CLI coding agent.
   Editors, file trees and so on come later, and only if we actually miss them.
5. **Agent-agnostic underneath, Claude first.** We ship with Claude Code. Nothing in the core
   should assume Claude, so opencode (or any other CLI agent) can be added as configuration
   rather than a rewrite.
6. **Ports and adapters.** The core knows nothing about UI, OS or agent details. Anything that
   might be swapped (how agents are hosted, where config lives, how engines are found) sits
   behind a port. See [Architecture](#architecture).

## How agents are hosted

This is the key architectural decision. There are three options:

| Option | How it works | Pros | Cons |
|---|---|---|---|
| **A. Terminal host** | Run the agent's own TUI (`claude`, `opencode`) in an embedded terminal (PTY) | Works with *any* CLI agent with no changes. You get the full native experience (slash commands, permissions, MCP). The least UI to build. | Chat is a character grid, so the IDE can't easily "see" or react to messages |
| **B. ACP client** | Talk to the agent over [Agent Client Protocol](https://agentclientprotocol.com) (JSON-RPC on stdio) and render our own chat UI | Structured messages, tool calls, diffs and permission prompts. One protocol covers Claude (through the `claude-agent-acp` adapter), opencode (`opencode acp`, built in), Codex, Gemini and others. | We have to build a full chat UI. Claude goes through an adapter (Node). Some features lag behind the native TUI. |
| **C. Claude headless** | `claude -p --input-format stream-json --output-format stream-json` | Structured output, no adapter | Claude-specific, and the input format is thinly documented |

**Decision: A ships first. B is a future adapter, for non-Claude agents only. C is ruled out.**

- **Terminal (A)** is the only adapter in the MVP. Tethys starts the agent's own CLI in an
  embedded terminal, with the project root as its working directory, and doesn't parse,
  wrap or restyle its output. Any CLI agent works this way just by adding a config entry.
- **ACP (B)** comes later as a second adapter behind the same port (see
  [Architecture](#architecture)). It's for agents that support ACP natively and don't sign in
  with a Claude subscription, starting with `opencode acp`. Switching an agent between
  terminal and ACP is a config change.
- **Claude always uses the terminal adapter.** Claude over ACP goes through
  `claude-agent-acp`, which is built on the Agent SDK. Anthropic's subscription terms (checked
  2026-10-08 against the
  [legal & compliance page](https://code.claude.com/docs/en/legal-and-compliance)) say Agent SDK
  products should use an API key, not a Pro/Max subscription. Running the **unmodified `claude`
  binary**, signed in through Anthropic's own flow, is explicitly allowed with a subscription.
  For the same reason, Claude headless mode (C) is ruled out.
- **Hard rules:**
  - Always run the agent exactly as installed (no patched binaries, no injected credentials).
  - Tethys never handles, stores or proxies Claude credentials.
  - Never have a third-party agent sign in with a Claude subscription.

```toml
# Sketch of agent config (user-level, e.g. %APPDATA%/tethys/agents.toml)
[[agent]]
id = "claude"
name = "Claude Code"
adapter = "terminal"        # the only adapter that's allowed for Claude
command = "claude"
args = []

# later — same agent, either adapter:
# [[agent]]
# id = "opencode"
# name = "opencode"
# adapter = "acp"           # or "terminal" with args = []
# command = "opencode"
# args = ["acp"]
```

When Tethys loads the config, it rejects `adapter = "acp"` for any entry whose command
basename is `claude`. This is a guard rail against mistakes, not a security boundary: a renamed
binary defeats it, and that's fine. The rule is there so nobody does it by accident.

## Stack

All versions checked on crates.io on 2026-10-08.

| Concern | Choice | Version | Why |
|---|---|---|---|
| UI framework | **GPUI** (from Zed) via **gpui-kit** | gpui-kit 0.7.1 (2026-10-05), which pins `gpui-pre =0.3.8` | Built for a fast code editor. GPU rendered and retained mode. Runs Zed in production on Windows, macOS and Linux. Zed's own terminal is GPUI + `alacritty_terminal`, which is exactly the combination we need. gpui-kit is a single dependency that bundles a pinned GPUI plus gpui-component (dock/tab, list, input and theme layer). |
| Terminal emulation | **alacritty_terminal** | 0.26.0 | The standard VT parser and grid, used by Zed and others. It ships its own ConPTY (Windows) and Unix PTY backends. |
| PTY (fallback) | portable-pty | 0.9.0 | Only if alacritty's tty module doesn't work for us. It hasn't been released since 2025-02. |
| `.uproject` parsing | serde + serde_json | — | `.uproject` is plain JSON |
| File dialogs | rfd | 0.17.2 | Native open dialog |
| Windows registry (engine lookup) | winreg | 0.56.0 | Resolve `EngineAssociation` to an install path |
| File watching (later) | notify | 8.2.0 | Watch the `.uproject` and Source for changes |
| ACP adapter (later, non-Claude only) | agent-client-protocol | 3.1.0 (2026-10-07) | The official Rust SDK. Implement its `Client` trait inside the ACP adapter crate. |

### Why GPUI and not egui or iced

- **egui** (0.36) is the quickest to get running, but it redraws in immediate mode, ends up
  looking like a debug tool, and its terminal widget (`egui_term` 0.1.0, April 2025) is
  self-described as incomplete.
- **iced** (0.14, December 2025) is solid, and `iced_term` exists. But its releases are slow
  and nothing like an IDE has been built on it at Zed's scale.
- **GPUI** is the one framework here that was built for an IDE and proven in one. The main
  risk is churn: the plain `gpui` crate on crates.io is stale (0.2.2, 2025-10). `gpui-kit`
  solves this by pinning `gpui-pre`, Longbridge's republished build of upstream GPUI
  (0.3.8 as of 2026-10-05). We lock `gpui-kit` to an exact version.
- **Fallback:** if GPUI's churn hurts too much, egui + `egui_term` is the plan B. The UI layer
  will be thin enough to swap. The M0 spike (see [Milestones](#milestones)) is where we find
  out: **if a terminal grid isn't rendering in GPUI on Windows after two days, switch to egui.**

### Why not C++

Qt or Dear ImGui would work. Rust gives us memory safety, cargo, and a library ecosystem
(alacritty_terminal, GPUI) that lines up with this exact problem. The project will be easier
to maintain in Rust.

## Architecture

**Hexagonal (ports and adapters).** The core holds the domain (projects, agent profiles,
sessions) and the use cases. It talks to the outside world only through **ports**, which are
Rust traits the core owns. **Adapters** implement those ports in separate crates. Dependencies
only point inward: adapters depend on the core, never the other way round. Cargo enforces this,
because `tethys-core` has no dependency on GPUI, alacritty, winreg or any adapter crate.

```
                    ┌──────────────── driving side ────────────────┐
                    │  tethys-app (GPUI UI, CLI args, composition) │
                    └──────────────────────┬───────────────────────┘
                                           │ calls use cases
                    ┌──────────────────────▼───────────────────────┐
                    │                 tethys-core                  │
                    │  domain: Project, AgentProfile, Session      │
                    │  use cases: open_project, start_session, …   │
                    │  ports: EngineLocator, ConfigStore,          │
                    │         AgentHost                            │
                    └──────────────────────┬───────────────────────┘
                                           │ implemented by
                 ┌─────────────────────────┼─────────────────────────┐
          engine_registry              config_toml              agent_terminal        (later: agent_acp,
          (winreg)                      (toml, %APPDATA%)        (alacritty_terminal)   non-Claude only)
                              all modules of the tethys-adapters crate
```

### Domain

- A **Project** is a parsed `.uproject` plus its resolved engine path.
- An **AgentProfile** is a named command line plus the adapter that should run it.
- A **Session** is one agent process tied to one project, with its working directory set to the
  project root. A project can have several sessions (tabs).

`.uproject` parsing lives **in the core**. It's pure domain logic (string in, `Project` out),
fully testable from string literals, and the only I/O is one `read_to_string` at the edge. It
doesn't need a port.

### Ports

| Port | What the core needs | MVP adapter | Later / test adapters |
|---|---|---|---|
| `EngineLocator` | Turn `EngineAssociation` into an engine install path | `engine_registry` (winreg + directory walk) | Fake for tests. Non-Windows lookup if we ever need it. |
| `ConfigStore` | Agent profiles, recent projects | `config_toml` | In-memory fake |
| `AgentHost` | Start, stop and watch an agent session in a project | `agent_terminal` (PTY + alacritty_terminal) | `agent_acp` (opencode). Fake for tests. |

The core's `AgentProfile.adapter` field chooses which `AgentHost` adapter runs a session. The
composition root in `tethys-app` registers the available hosts. The core rejects Claude + ACP,
so that rule lives in the domain and not in the UI.

```rust
// tethys-core::ports (sketch)
pub trait AgentHost {
    fn kind(&self) -> AdapterKind;                       // Terminal | Acp
    fn start(&self, profile: &AgentProfile, project: &Project,
             events: Sender<SessionEvent>) -> Result<Box<dyn AgentSession>>;
}

pub trait AgentSession: Send {
    fn status(&self) -> SessionStatus;                   // Starting | Running | Exited(code)
    fn stop(&mut self) -> Result<()>;
}

pub enum SessionEvent { Started, TitleChanged(String), Bell, Exited(i32) }
```

Sessions **push** events through the channel the core hands them. The UI doesn't poll.

**Rendering a session.** The terminal adapter's output is a character grid. A future ACP
adapter's output is structured chat. These are different enough that pushing both through one
"render" abstraction would be a leaky lowest common denominator. So the port covers only the
session **lifecycle**. Each adapter comes with a matching **view** in `tethys-app`, either a
terminal view or (later) a chat view, and the UI picks the view based on `AdapterKind`. The core
never deals with pixels or cells.

**How the view reaches the grid.** The port hides alacritty's `Term` from the core on purpose,
but the GPUI terminal view needs it to draw cells. `tethys-app` is the composition root, so it's
allowed to depend on `tethys-adapters` directly. The terminal adapter exposes a concrete
`TerminalHandle` alongside its `AgentHost` impl. The app creates the adapter, gives the core the
trait object, and keeps the concrete handle for the view. Nobody downcasts and no alacritty type
leaks into `tethys-core`.

### Crate layout

```
tethys/
├── crates/
│   ├── tethys-core        # domain (incl. .uproject parsing), use cases, port traits.
│   │                      #   Deps: serde, serde_json, thiserror. Nothing else.
│   ├── tethys-adapters    # engine_registry, config_toml, agent_terminal as modules.
│   │                      #   Depends on tethys-core. Split into crates only if compile times demand it.
│   └── tethys-app         # GPUI binary: composition root, views (project summary, terminal)
└── docs/
```

Three crates is the minimum that lets Cargo enforce "core depends on nothing".

### Keeping it MVP

- Only add a port where there's a real I/O boundary **and** a realistic second implementation or
  test double. Domain logic stays plain code, not hidden behind traits.
- Each port ships with **one** real adapter. `agent_acp` isn't written until we actually
  want opencode over ACP. The port is designed so that adding it later is cheap, but we don't
  build it ahead of time.
- Test the core with fake adapters, without any UI or processes.

### Terminal adapter notes

Things we know will bite in M2, written down now so they aren't rediscovered:

- **Process trees.** `claude` spawns children. On Windows, closing the PTY doesn't kill them.
  Put each session in a **Job Object** with kill-on-close, so closing a tab or quitting Tethys
  takes the whole tree down.
- **npm shims.** `claude` on Windows is a `.cmd` shim. Either resolve the real executable or
  spawn through `cmd /c`. Inherit the user's full environment, or Claude won't find Git Bash
  (`CLAUDE_CODE_GIT_BASH_PATH`) and its own config.
- **Working directory** is always the directory containing the `.uproject`.

### `.uproject` handling

A `.uproject` is JSON. The fields we care about at first are:

- `EngineAssociation`, which has three forms:
  - A version string (`"5.6"`): a Launcher install. Look up
    `HKLM\SOFTWARE\EpicGames\Unreal Engine\<ver>\InstalledDirectory`.
  - A GUID: a registered source build. Look up the value of that name under
    `HKCU\Software\Epic Games\Unreal Engine\Builds`.
  - **Empty**: a "native" project that lives inside a source engine tree. Walk up from the
    `.uproject` until a directory contains `Engine\Build\Build.version`. Studio source builds
    often look like this, so M1 must handle it.
- `Modules` and `Plugins`: shown in the project summary, and useful context for the agent later.

"Native opening" means, in order:

1. `tethys.exe path\to\Game.uproject` (CLI argument)
2. File → Open (an rfd dialog filtered to `*.uproject`) and drag-and-drop onto the window
3. *Later:* an **"Open with Tethys"** entry on the right-click menu for `.uproject` files.
   This is deliberately **not** the default double-click action, because that belongs to
   UnrealVersionSelector and we shouldn't break it.

## Milestones

Each milestone is the smallest useful step. M0 carries a spike that isn't a feature: it exists to
retire the one risk the whole stack depends on (terminal rendering in GPUI on Windows) before
M1 is built on top of it.

| # | Deliverable | Done when |
|---|---|---|
| **M0** | Cargo workspace, GPUI window opens, CI builds on Windows. **Plus a throwaway spike:** `cmd.exe` running in an alacritty_terminal grid drawn by GPUI, no polish. | `cargo run` shows a Tethys window with a working shell in it. The spike code is deleted or rewritten in M2. |
| **M1** | Open a `.uproject` (CLI argument and dialog). Show project name, engine association and resolved engine path. | Opening ArcadeDemo shows correct info. Invalid files give a clear error. |
| **M2** | One Claude Code session in a terminal pane, started in the project root | Run a real task with `claude` inside Tethys, end to end |
| **M3** | Several sessions as tabs. Remember recent projects. | Two Claude sessions on one project at once |

**Candidates after M3, in no fixed order and only when needed:** opencode profile
(terminal); `agent_acp` adapter + chat view for opencode; an agent picker for new sessions; Explorer "Open with Tethys"; buttons to build or launch the
editor (UBT / `UnrealEditor.exe`); feeding project context (modules, plugins, engine path) to
the agent; a read-only file viewer.

## Open questions

- Is Windows the only target for now? (We assume yes, but GPUI keeps macOS and Linux possible.)
- Do sessions need to survive an IDE restart (`claude --resume`), or are fresh sessions fine for the MVP?
- What happens to running sessions when Tethys closes: kill them (the Job Object default), or warn first?
- Should Tethys inject anything into the agent at startup (e.g. a Tethys-specific `CLAUDE.md` or MCP server), or leave the project's own config alone?

## Prerequisites

- Rust via rustup: `winget install Rustlang.Rustup`, then the MSVC toolchain.
- Rust needs the MSVC linker. The Visual Studio C++ toolchain that UE development needs should already provide it.
