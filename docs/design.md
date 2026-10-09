# Tethys — High-Level Design

> Status: **M0–M3 implemented** · 2026-10-08 (see [Implementation notes](#implementation-notes))

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
  will be thin enough to swap. **Result:** the M0 spike worked first time (an alacritty grid
  drawn by GPUI, running `claude` through ConPTY on Windows), so the fallback isn't needed.

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
| `Debugger` | Attach to the running editor, set breakpoints, report stops (see [Debugging the editor](#debugging-the-editor)) | `debug_dbgeng` (DbgEng) | Fake for tests. |

The core's `AgentProfile.adapter` field chooses which `AgentHost` adapter runs a session. The
composition root in `tethys-app` registers the available hosts. The core rejects Claude + ACP,
so that rule lives in the domain and not in the UI.

```rust
// tethys-core::ports
pub type EventSink = Arc<dyn Fn(SessionId, SessionEvent) + Send + Sync>;

pub trait AgentHost {
    fn kind(&self) -> AdapterKind;                       // Terminal | Acp
    fn start(&self, id: SessionId, profile: &AgentProfile, project: &Project,
             events: EventSink) -> PortResult<Box<dyn AgentSession>>;
}

pub trait AgentSession: Send {                           // dropping it stops the agent
    fn id(&self) -> SessionId;
    fn status(&self) -> SessionStatus;                   // Starting | Running | Exited(code)
    fn stop(&mut self) -> PortResult<()>;
}

pub enum SessionEvent { Started, TitleChanged(String), Bell, Exited(i32) }
```

Sessions **push** events through the sink the core hands them, from any thread. The UI doesn't
poll. The sink is a callback rather than a channel so the core doesn't pick a channel type; the
app forwards it into its own async channel.

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
leaks into `tethys-core`. Concretely, the app starts a session through the core, then calls
`TerminalHost::take_handle(id)` on its own concrete host to get the handle for that session.

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
  - A version string (`"5.6"`): a Launcher install.
  - A GUID: a registered source build.

  Both forms are looked up the way UE's own tools do it
  (`FDesktopPlatformWindows::EnumerateEngineInstallations`), in this order:
  1. The Epic Launcher's install list, `%ProgramData%\Epic\UnrealEngineLauncher\LauncherInstalled.dat`
     (app `UE_<ver>`). This is UE's main source for Launcher engines.
  2. `HKCU\Software\Epic Games\Unreal Engine\Builds`, by value name (any name, usually a GUID).
  3. `HKLM\SOFTWARE\EpicGames\Unreal Engine\<ver>\InstalledDirectory`.

  The first recorded folder that actually holds an engine (`Engine\Binaries`) wins. Nothing
  else about engine paths is assumed, so each machine resolves its own install location.
  (Until 0.2.1 Tethys read only the `HKLM` key, so a machine with the engine in the Launcher's
  list but a missing or stale key couldn't find it.)
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

## Debugging the editor

> Status: **MVP implemented 2026-10-09, plus launching under the debugger.** Checked against a
> live UE 5.8 editor (AeonixDemo): it attaches in about 0.7 s, an engine breakpoint binds in
> about 0.6 s and is hit on the right line, pausing shows the game thread with engine symbols,
> and the editor keeps running after detaching. Launched under the debugger, the editor starts
> in 0.25 s, a breakpoint set beforehand binds as its module loads and is hit with no other
> stops on the way (`cargo test -p tethys-adapters live_editor -- --ignored`, see the test for the
> variables it needs). Not yet tried live: the Tethys UI as a whole, stops on `check` and
> `ensure` (the editor's `debug ensure` console command triggers one), and access violations.

Tethys attaches a native debugger to the running Unreal Editor, or starts the editor under it.
It catches crashes, `check`s and
`ensure`s, shows the stack of the thread that stopped, and stops at source-line breakpoints set
in Tethys's editor. That covers most of what UE programmers use Visual Studio's debugger for while
working with an agent.

### Which debugger engine

| Option | Verdict |
|---|---|
| **Visual Studio's debugger** (Concord / `vsdebugeng`, or `vsdbg` from VS Code's C++ extension) | **Ruled out.** `vsdbg`'s licence only allows use with Microsoft's own products, and Microsoft enforces it against third-party editors. Concord can't be hosted outside Visual Studio. |
| **DbgEng** (`dbgeng.dll`, the engine behind WinDbg and cdb) | **Chosen.** It's the most reliable engine for MSVC binaries and PDBs, and it ships in every Windows install (`System32`), so Tethys doesn't redistribute anything. It has a documented COM API, which the `windows` crate (MIT/Apache) binds. It also loads `.natvis` files, so UE's `Unreal.natvis` is available when we add variable inspection. |
| **lldb-dap** over the Debug Adapter Protocol | Later, if ever. LLDB's PDB support for MSVC is still rough, it can't read natvis, and it hasn't been proven at UE's scale (about 700 DLLs and gigabytes of PDBs). |
| **Drive Visual Studio through DTE** (COM automation) | Not needed for now. It's legal, but debugging would happen in Visual Studio, not in Tethys. It could be a cheap "Debug in Visual Studio" button later. |
| **RAD Debugger** (Epic, MIT) | A complete app, not an engine we can embed. It could be a "launch external debugger" option later. |

### Port

The core owns the vocabulary and the decisions. The adapter only talks to DbgEng.

```rust
// tethys-core::ports
pub trait Debugger {
    /// Running processes, so the core can pick the project's editor.
    fn processes(&self) -> PortResult<Vec<ProcessInfo>>;
    /// Both set `breakpoints` before the program runs on, so startup code can be debugged.
    fn attach(&self, pid: u32, breakpoints: Vec<SourceBreakpoint>, events: DebugEventSink)
        -> PortResult<Box<dyn DebugSession>>;
    fn launch(&self, command: &CommandSpec, cwd: &Path, breakpoints: Vec<SourceBreakpoint>,
              events: DebugEventSink) -> PortResult<Box<dyn DebugSession>>;
}

pub trait DebugSession: Send {          // dropping it detaches; it never kills the editor
    fn pid(&self) -> u32;
    fn set_breakpoints(&self, breakpoints: Vec<SourceBreakpoint>);  // the full desired set
    fn resume(&self);
    fn pause(&self);
    fn detach(&self);
}

pub enum DebugEvent {
    Attached,
    Running,
    Stopped { reason: StopReason, thread: u32, frames: Vec<StackFrame> },
    Breakpoints(Vec<(BreakpointId, BreakpointState)>),   // Bound | Pending | Failed
    Ended(DebugEnd),                                     // Detached | Exited | Failed(msg)
}
```

Like `AgentSession`, a session **pushes** events through a sink from its own thread.
`set_breakpoints` always sends the full set of breakpoints Tethys wants. The adapter replaces
what it has set with it, so it never drifts from the UI.

The core's `debug` module holds the plain logic, all of it unit-tested:

- **Finding the editor:** `find_editor` picks an `UnrealEditor*.exe` whose command line names
  this project's `.uproject`. If none does and only one editor is running, it picks that one.
- **Attach or launch:** the `debug_editor` use case attaches to that editor. If none is running
  it launches one (`unreal::editor_command`, so in the chosen configuration) under the debugger.
  It can also be told to only attach or only launch.
- **The module that owns a source file:** UE compiles each module into its own DLL
  (`UnrealEditor-<Module>.dll`, or `UnrealEditor-<Module>-Win64-DebugGame.dll`). The module
  is named after the nearest folder above the file that holds a `<Folder>.Build.cs`.
  `module_for_source` walks up from the file, using a callback to check whether the
  `Build.cs` exists.
- **Which DLL is a module's:** `is_module_image` matches a loaded image against
  `UnrealEditor-<Module>[-<Platform>-<Config>]`. The adapter qualifies each breakpoint with that
  module, so DbgEng only has to load that one module's PDB. Without the module, it would search
  the symbols of every loaded module.
- **Source paths in stack frames:** engines installed from the Launcher record paths from
  Epic's build machine (e.g. `D:\build\++UE5\Sync\Engine\Source\…`). `local_source` maps any
  path containing an `Engine\` segment onto the local engine root when the original path
  doesn't exist.

### DbgEng adapter (`tethys-adapters::debug_dbgeng`)

- **One thread owns the engine.** DbgEng's COM objects aren't thread-safe, so a dedicated thread
  creates the client and makes every call. The UI talks to it through a channel.
- **Event loop.** While the editor runs, the thread calls `WaitForEvent` with a 100 ms timeout
  and handles commands between waits. `S_FALSE` means the wait timed out. The `windows` crate
  folds `S_FALSE` into `Ok`, and the execution status can read `BREAK` after a timeout, so the
  adapter calls the vtable directly to see the raw HRESULT. A command that needs the target
  stopped (changing breakpoints, detaching) first breaks in, applies the change, and resumes,
  without the UI ever seeing a stop. `Pause` uses the same break-in but reports `Stopped(Pause)`
  with the main (game) thread's stack.
- **Breaking in** uses Win32 `DebugBreakProcess`, not DbgEng's `SetInterrupt`. `SetInterrupt`
  does nothing when it's called from the engine's own thread while that thread isn't waiting.
  `DebugBreakProcess` makes the OS start a thread at `ntdll!DbgUiRemoteBreakin`, which hits a
  breakpoint. The attach break and a new process's loader breakpoint are similar, but they
  don't always stop in `DbgUiRemoteBreakin` (tried: one attach stopped in thread start-up code
  in ntdll). So a breakpoint stop with **only OS frames** (ntdll, kernel32, KernelBase) on the
  stack is ours and is never shown. A real `check`, `ensure` or `__debugbreak` has program code
  on the stack.
- **Attach.** `AttachProcess` with `DEBUG_ENGOPT_INITIAL_BREAK`, so the first stop is
  predictable. The thread then sets breakpoints and resumes. `DEBUG_PROCESS_DETACH_ON_EXIT` is
  set straight after the attach break, so **if Tethys exits while the editor runs, the editor
  keeps running**. If Tethys dies while the editor is *stopped*, though, the editor stays frozen:
  DbgEng suspended its threads, and nothing resumes them. Resuming them by hand isn't enough
  either (tried: the thread at the breakpoint crashes). Closing a window detaches cleanly first:
  dropping a session sends Detach and waits up to 5 s for it.
- **Launch.** `IDebugClient5::CreateProcessAndAttach2Wide` with `DEBUG_ONLY_THIS_PROCESS`, so
  the editor's children (shader compilers, the crash reporter) aren't debugged. The first stop
  is the loader's breakpoint. Processes created under a debugger get the slow NT debug heap,
  so the editor gets Tethys's environment plus `_NO_DEBUG_HEAP=1`. DbgEng's own
  `DEBUG_CREATE_PROCESS_NO_DEBUG_HEAP` flag makes the System32 engine fail its first wait, so
  it isn't used. Detaching from a launched editor leaves it running, as for an attach.
- **One session per Tethys process.** DbgEng is one engine per process, not per client: a
  second session at the same time breaks the first (tried). The adapter refuses a second session
  or a process listing while one runs, with a message to detach in the other window.
- **Stopping.** The thread reads the event with `GetLastEventInformation`:
  - **Exceptions:** an exception is reported if DbgEng's filters say to break on it. By default
    that means first-chance access violations, breakpoint instructions and every second-chance
    exception. UE's `check` and `ensure` call `__debugbreak()` when a debugger is attached, so
    they stop here too.
  - **Breakpoints:** a stop at one of our breakpoints is reported with its ID.
  - **Stack:** the stack of the thread that stopped comes from `GetStackTrace`, then
    `GetNameByOffset` and `GetLineByOffset` for each frame. For frames below the top, the
    lookup uses the return address minus one, so the line is the call, not the line after it.
- **Breakpoints** are added with ``bu<id> `<module>!<file>:<line>` `` through `Execute`, using
  IDs that Tethys chooses. This is the unresolved form, so a breakpoint in a module that hasn't
  loaded yet binds when the module loads. After setting them, the adapter reads each
  breakpoint's flags: `DEBUG_BREAKPOINT_DEFERRED` means **Pending**, an `Execute` failure means
  **Failed**, and otherwise it's **Bound**. The module name comes from the loaded module whose
  image is `UnrealEditor-<Module>[-Win64-<Config>].dll`. If none is loaded, the name is
  guessed (`UnrealEditor_<Module>`), and the guess is wrong for DebugGame project modules. So
  while any breakpoint is Pending, the engine stops on every module load (the `ld` event
  filter). If the new DLL is a waiting breakpoint's module, the breakpoints are set again,
  this time with its real name. Otherwise the editor continues straight away. The filter is off
  again once nothing is waiting.
- **Breakpoint objects are owned by the engine.** `GetBreakpointById` returns an
  `IDebugBreakpoint` that DbgEng doesn't reference-count, so releasing it frees it, and releasing
  it after `RemoveBreakpoint` crashes Tethys. The adapter wraps them in `ManuallyDrop` and never
  releases them.
- **Symbols** come from the PDBs next to each module, which is where UBT writes them, plus
  `_NT_SYMBOL_PATH` if it's set. The folders of loaded modules are added to the symbol path
  before every stack walk and breakpoint change, because a launched editor loads its DLLs
  after the first stop. Line information is turned on (`SYMOPT_LOAD_LINES`) and
  loading is deferred, so the first stop in a module loads its PDB. That can take a few seconds
  for `UnrealEditor-Engine`.
- **Detach** stops the editor first (so DbgEng takes its breakpoints out of the code), then
  calls `DetachProcesses` and `EndSession(DEBUG_END_PASSIVE)`. The editor keeps running.

### UI

- **Debug button** in the project header. If no debugger is attached, it attaches to this
  project's running editor, or launches the editor under the debugger if none is running.
  Otherwise it detaches. The Debug menu also has "Launch Editor with Debugger" and "Attach to
  Running Editor".
- **Debug panel** at the bottom of the dock:
  - A status line: attached to which process ID, and whether the editor is running or stopped,
    and why.
  - Buttons: Continue, Pause and Detach.
  - The stack of the thread that stopped: click a frame to open its source at that line.
  - The breakpoint list, with each breakpoint's state; click one to open it, or remove it.
- **Keys:** F5 debugs (as the Debug button), or continues after a stop. Shift+F5 detaches. Terminal panes keep
  F-keys for the agent, so use the buttons or the Debug menu when a terminal has focus.
- **Editor:** F9 or a toolbar button toggles a breakpoint on the cursor's line. Breakpoint lines
  get a red fill and the current stop line gets an orange one, both through the editor's range
  decorations. Breakpoints belong to the window, not to an attach, so they persist across
  attach and detach, but they aren't saved to disk yet.

### MVP scope

**In:** attach to the running editor or launch it under the debugger, stop on exceptions, `check` and `ensure`, show the stack
of the thread that stopped, file:line breakpoints, continue, pause and detach.

**Later, in rough order:**
- Stepping (over, into, out).
- A thread list.
- Locals and watches, using `Unreal.natvis` through DbgEng's data model.
- Saving breakpoints in `state.toml`.
- Breakpoints on Live Coding patches.
- Showing the crash stack from `Saved/Crashes` when no debugger is attached.

**Known limits:** breakpoint lines don't move when lines are added or removed above them.
Breakpoints in headers bind only in the module that owns the header, not in
every module that inlines it. A Launcher engine only has engine symbols if the user installed
"Editor symbols for debugging", so without them engine frames have no names.
When the editor is started under the debugger, UE skips its own crash handler, so crashes stop
in the debugger instead of opening the crash reporter. A crash in an attached editor still stops
first, as a first-chance exception.

## Live Coding

A **Live Coding** button in the project header (and Build → Live Coding) recompiles changed C++
into the running editor. While the editor runs, Live Coding is the only way to build: UBT
refuses to build while a Live Coding session is active.

- **How it's triggered:** the `LiveCoding` port's `live_coding_hotkey` adapter presses UE's
  Live Coding shortcut, Ctrl+Alt+F11, with `SendInput`, holding it for 100 ms. Both of UE's
  implementations watch the keyboard globally. `LiveCodingModule2` uses raw input with
  `RIDEV_INPUTSINK`. The older Live++ server polls `GetAsyncKeyState` every 10 ms. So the editor
  doesn't need focus, and no plugin or project setting is needed. Checked on UE 5.8: the
  editor log shows `Starting Live Coding compile.`
- **Alternatives not taken:** the `LiveCoding.Compile` console command needs a way into the
  editor's console, such as the Remote Control or Python remote-execution plugins, which
  projects don't enable by default.
- **Focus:** the shortcut also reaches Tethys's own window, so the button first moves focus off
  any terminal, which would otherwise pass F11 to the agent.
- **Limits:**
  - A shortcut changed in the editor's Live Coding settings isn't followed.
  - It only checks that some Unreal Editor is running, not that it's this project's.
  - It refuses while the debugger has the editor stopped, since the compile would wait for it.
  - Progress and errors show in the editor's own Live Coding window, not in Tethys.

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

**Status (2026-10-08):** M0–M3 are implemented. There was no separate throwaway spike: the
terminal view was written properly from the start, since the spike worked first time.
Tested by hand so far: opening ArcadeDemo (from the CLI argument and from the recent list),
the engine resolving to `C:\Engines\UE_5.8`, Claude starting in the project root and rendering, typing,
a clear error for a broken `.uproject`, and Claude exiting cleanly when the window closes.
Still to test by hand: the file dialog, drag-and-drop, two sessions at once, copy/paste, and a
full real task.

**Candidates after M3, in no fixed order and only when needed:** opencode profile
(terminal); `agent_acp` adapter + chat view for opencode; an agent picker for new sessions; Explorer "Open with Tethys"; buttons to build or launch the
editor (UBT / `UnrealEditor.exe`); feeding project context (modules, plugins, engine path) to
the agent; a read-only file viewer.

## Open questions

- Is Windows the only target for now? (We assume yes, but GPUI keeps macOS and Linux possible.)
- Do sessions need to survive an IDE restart (`claude --resume`), or are fresh sessions fine for the MVP?
- ~~What happens to running sessions when Tethys closes?~~ Closing a tab or window closes the
  agent's console first (Windows sends it `CTRL_CLOSE_EVENT`, so it can clean up), then kills
  whatever is still in its Job Object after 3 seconds. Tethys waits for this before quitting.
  There's no warning yet.
- Should Tethys inject anything into the agent at startup (e.g. a Tethys-specific `CLAUDE.md` or MCP server), or leave the project's own config alone?

## Prerequisites

- Rust stable (MSVC target) via rustup.
- Rust needs the MSVC linker. The Visual Studio C++ toolchain that UE development needs should already provide it.

## Implementation notes

Things learned while building M0–M3 that aren't obvious from the code.

- **Stopping agents gently.** A hard kill (`TerminateJobObject`, or `taskkill /F`) makes Claude
  Code think its fullscreen renderer crashed, so the next launch falls back to the classic
  renderer. `TerminalSession::stop` closes the ConPTY first and only kills the job after a grace
  period.
- **Block characters** (U+2580–U+259F, e.g. Claude's logo) are drawn as rectangles, not font
  glyphs, so they tile without gaps at our line height.
- **Grid alignment.** Each run of same-styled cells is shaped with `force_width` set to the cell
  width and painted at its column. Wide characters get their own run.
- **Command resolution.** CreateProcess only finds `.exe` files. The adapter searches `PATH` with
  `PATHEXT` itself and runs `.cmd`/`.bat` shims through `cmd /d /s /c`.
- **Environment.** Sessions inherit Tethys's environment plus `TERM=xterm-256color` and
  `COLORTERM=truecolor`. If Tethys is launched from inside a Claude Code session, the child Claude
  inherits its `CLAUDE_CODE_*` markers and warns that transcript saving is off. Launch Tethys from
  Explorer or a plain terminal.
- **Keys.** Ctrl+Shift+… is reserved for Tethys (O open, T new session, W close session) and
  Ctrl+Tab / Ctrl+Shift+Tab switch tabs. Everything else goes to the agent. Ctrl+C copies when
  there's a selection; Ctrl+V pastes text, or sends `^V` when the clipboard has no text so Claude
  can paste images. Shift+Enter sends `ESC CR` (a newline in Claude Code). Right-click pastes.
- **Windows.** One project per window. Opening a second project opens a second window.
- **Build and launch (post-M3).** `tethys-core::unreal` builds the command lines as pure
  functions. A build runs `Build.bat <Target> Win64 Development -Project=… -WaitMutex` as a
  terminal session through the same `AgentHost`, so UBT output is live and the exit code marks
  the tab ✓/✗. Only one build runs at a time. The editor target is `<Name>Editor` if
  `Source/<Name>Editor.Target.cs` exists, otherwise the first `*Editor` target. The editor
  starts through a new `ProcessLauncher` port as a detached process, outside any Job Object, so
  it outlives Tethys.
- **Build configuration.** Development or DebugGame, picked in the project header and saved in
  `state.toml` (`ConfigStore::build_configuration`). DebugGame builds with
  `Build.bat … DebugGame` and launches `Engine\Binaries\Win64\UnrealEditor-Win64-DebugGame.exe`.
  Installed engines ship that executable; it loads the project's `-Win64-DebugGame` modules.
- **Settings.** File → Settings… (Ctrl+,) opens an app-wide dialog (`settings_ui.rs`). It
  currently has one setting: **extra command-line arguments for the editor**. They're added
  after the `.uproject` whenever Tethys starts the editor, whether through Launch editor or
  through Debug when Debug launches it.
  - They're stored as typed in `state.toml` (`ConfigStore::editor_args`) and split at launch by
    `unreal::split_args` (spaces separate arguments, double quotes group them).
  - Until the user saves a value, the default `unreal::DEFAULT_EDITOR_ARGS`
    (`-ModelContextProtocolStartServer`) applies. That switch starts UE 5.8's Unreal MCP server
    (the experimental `ModelContextProtocol` plugin) with the editor, and the editor ignores it
    when the plugin isn't enabled.
  - Saving an empty value means no extra arguments, not the default.
- **Docking.** Sessions (Claude and builds) are panels in a gpui-component `DockArea`
  (`SessionPanel`), so tabs can be dragged into splits and resized. The panel owns its
  `AgentSession`: closing its tab calls `on_removed`, which stops the process tree. The layout
  isn't saved yet.
- **Agent choice.** Without an `agents.toml`, the built-in profiles are Claude Code and opencode,
  both on the terminal adapter. "New session ▾" lists every profile and Ctrl+Shift+T starts the
  first. For full-screen TUIs like opencode, the terminal sends mouse-wheel reports (SGR or
  legacy) when the program enables mouse mode, and arrow keys on the alternate screen. Mouse
  clicks aren't forwarded yet.
- **Menu bar.** File / Build / Help, drawn in-window by gpui-component's `AppMenuBar` from
  menus set in `install_menus`. Menu items dispatch the same actions as the shortcuts. Exit
  closes every window, which stops their sessions gracefully. Help → About opens a dialog. The
  app registers an asset source (`assets.rs`) with gpui-kit's default icons plus the few Lucide
  icons the toolbar uses.
- **Build pane.** Each project window has a build pane in the dock's right region
  (`DockPlacement::Right`, 560px to start, collapsible and resizable). Builds always open there
  and agents open in the center. Before the first build, and after the last build tab closes, it
  holds a `BuildPlaceholder` that can't be closed. Starting a build closes finished build tabs, so
  the pane shows the latest log.
- **Files, editor and diff.** A `SourceControl` port (`working_copy_root`, `info`, `status`, `base_text`)
  with a Subversion adapter (`scm_svn`) that runs the `svn` CLI read-only: `status --xml` and
  `cat -r BASE`, which reads the pristine copy with no network access, and no console windows.
  A git adapter (`scm_git`) does the same with the `git` CLI: `status --porcelain=v2 -z`
  limited to the project folder (which may be below the repository root), and
  `cat-file --filters HEAD:<path>` for the base, so `core.autocrlf` line endings match the
  working file. Every git command runs with `--no-optional-locks`, so a status refresh never
  writes the index while an agent is using git. Each project uses whichever source control
  manages it (`detect_source_control`: the nearest working copy root wins, git first on a tie).
  The core's `diff` module turns the base and the current text into hunks (with `similar`).
  - **Files pane:** loads folders lazily and colours them from `WorkingCopyStatus`. Its footer shows the provider and where the working copy points (`SourceControl::info`, local only: `svn info --xml`, or for git the branch, HEAD, the upstream or origin URL and the last commit touching the project folder): branch, revision, URL and last change, or "No source control". An All / Changes toggle switches to a flat list of local edits (`WorkingCopyStatus::local_edits`: added, modified, replaced, conflicted). Status
    refreshes on project open, after each save, and when the window regains focus.
  - **Editor:** gpui-component's code editor. That editor can't draw tab stops, so `TextCodec`
    expands tabs for display and writes unedited lines back verbatim. Line endings and BOM are
    preserved too. Every text file in a large game's `Source` (707 files) round-trips byte for
    byte (`TETHYS_ROUNDTRIP_DIR=… cargo test -- --ignored`).
  - **Diff:** a unified diff against BASE (SVN) or HEAD (git) in the main area. Git renames show
    as the new path added and the old one deleted; the diff of a renamed file is against nothing.
  - **Not done yet:** nothing is ever committed, reverted or updated through source control; syntax
    highlighting is the editor library's default and fairly sparse; there's no prompt for unsaved
    edits when closing the window.
- **Visual identity.** Follows the Telesto Games brand (telesto.games):
  - **Colour:** plum `#0B001F` backdrop and broadcast orange `#F24B38` as the only accent, with
    plum text on orange buttons. Lilac `#C8B8D8` for quiet text. Structure is orange hairlines
    at low opacity, including a 1px line across the top of the window. Corners are nearly square.
  - **Type:** Cascadia Mono everywhere (SIL OFL, ships with Windows 11). The brand face, PP
    Fraktion Mono, is commercial and can't ship in an MIT product. Only use standard or
    OFL/MIT-compatible fonts and assets. Cascadia's capitals are centred in its line box
    (measured offset 0.000 em, against 0.065 em for Segoe UI), so labels sit level
    in buttons.
  - **Controls:** every control in the project header is `TOOLBAR_HEIGHT` (28px) tall and uses
    small labels: an outlined button for each secondary action, filled orange for the primary
    one, and a bordered pill for the configuration toggle.
  - **Labels:** GPUI has no letter-spacing, so `theme::tracked` imitates the brand's wide
    tracking for a few welcome-screen labels only. Panel tabs use plain title case.
  - Everything lives in `crates/tethys-app/src/theme.rs` and `agent_terminal::palette`.
- **App icon.** Tethys the moon (Telesto shares its orbit) in brand colours: an orange moon with
  a plum `>_` agent prompt above the Telesto horizon line, on a plum tile. The sources are
  `assets/icon/tethys.svg`, plus `tethys-small.svg` for 16–32 px, which drops the prompt so the
  icon stays legible. `tools/make-icon` (standalone, outside the workspace) renders them with
  resvg into `assets/icon/tethys.ico`. `crates/tethys-app/build.rs` embeds that as resource #1
  (via winresource), the resource GPUI uses for the window icon.
