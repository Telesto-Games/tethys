# tethys
Tethys: a lightweight, LLM-first IDE for Telesto Games' Unreal projects

Open a `.uproject` and Tethys starts Claude Code in the project folder, in a native
GPU-rendered terminal. See [docs/design.md](docs/design.md) for the design.

## Build and run

Needs Rust (stable, MSVC) and the Visual Studio C++ build tools.

```sh
cargo run -- path\to\Game.uproject     # or just `cargo run` and pick one
cargo build --release                  # target\release\tethys.exe, no console window
```

## Using it

| Do this | How |
|---|---|
| Open a project | Start `tethys.exe` with no argument for the project browser (picker + recent projects), or pass a `.uproject`. Also **Ctrl+Shift+O**, or drop a `.uproject` on the window |
| Project browser from a project | **Ctrl+Shift+P** opens it in a new window |
| New Claude session | **Ctrl+Shift+T** |
| Close session | **Ctrl+Shift+W** |
| Pick the build configuration | **Development** / **DebugGame** toggle in the project header (remembered between runs) |
| Build the editor target | **Ctrl+Shift+B**: runs `Build.bat <Project>Editor Win64 <Configuration>` in a tab (✓ or ✗ when done) |
| Launch the Unreal Editor | **Ctrl+Shift+E**: starts `UnrealEditor.exe` (or `UnrealEditor-Win64-DebugGame.exe`) on the project, independent of Tethys |
| Switch session | **Ctrl+Tab** / **Ctrl+Shift+Tab**, or click the tab |
| Copy | Drag to select, then **Ctrl+C** (or Ctrl+Shift+C) |
| Paste | **Ctrl+V** or right-click |

Every other key goes straight to the agent.

## Configuration

Optional. Without it, Tethys runs `claude` from `PATH`.

`%APPDATA%\tethys\agents.toml`:

```toml
[[agent]]
id = "claude"
name = "Claude Code"
adapter = "terminal"   # Claude may only use the terminal adapter
command = "claude"
args = []
```

The first profile is used for new sessions. Tethys keeps recent projects in
`%APPDATA%\tethys\state.toml`.
