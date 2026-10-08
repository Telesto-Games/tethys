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
| Project browser from a project | **File → Projects…** or **Ctrl+Shift+P** opens it in a new window |
| Menus | **File** (Open Project, Projects, New/Close Session, Exit), **Build** (Build, Launch Editor), **Help** (About) |
| New agent session | **New session ▾** → Claude Code or opencode. **Ctrl+Shift+T** starts the first (Claude Code) |
| Close session | **Ctrl+Shift+W** |
| Pick the build configuration | **Development** / **DebugGame** toggle in the project header (remembered between runs) |
| Build the editor target | **Ctrl+Shift+B**: runs `Build.bat <Project>Editor Win64 <Configuration>` in the **Build pane** on the right (✓ or ✗ when done). Each new build replaces the previous finished log |
| Launch the Unreal Editor | **Ctrl+Shift+E**: starts `UnrealEditor.exe` (or `UnrealEditor-Win64-DebugGame.exe`) on the project, independent of Tethys |
| Switch session | **Ctrl+Tab** / **Ctrl+Shift+Tab**, or click the tab |
| Browse files | The **Files** pane on the left lists the project folder. **All / Changes** at its top switches to a flat list of only added and modified files. Changed files are coloured (yellow modified, green added, red deleted/conflicted, grey unversioned) with their SVN letter; folders containing changes are yellow |
| Edit a file | Click it in Files. Ctrl+S saves, **Revert** discards unsaved edits. Line endings, BOM and tab indentation are preserved exactly. Tabs with unsaved edits can't be closed until saved or reverted |
| Diff a file | Click **diff** next to a changed file in Files, or **Diff** in its editor (includes unsaved edits). Shows a unified diff against the SVN BASE |
| Arrange sessions side by side | Drag a tab onto the edge of a pane to split it, or onto another tab bar to move it. Drag the divider to resize; close a tab with its × |
| Copy | Drag to select, then **Ctrl+C** (or Ctrl+Shift+C) |
| Paste | **Ctrl+V** or right-click |

Every other key goes straight to the agent.

## Configuration

Optional. Without it, Tethys offers Claude Code (`claude`) and opencode (`opencode`), both from `PATH`.

`%APPDATA%\tethys\agents.toml`:

```toml
[[agent]]
id = "claude"
name = "Claude Code"
adapter = "terminal"   # Claude may only use the terminal adapter
command = "claude"
args = []
```

Profiles appear in the New session menu; the first is the Ctrl+Shift+T default. opencode installs with `npm i -g opencode-ai`. Tethys keeps recent projects in
`%APPDATA%\tethys\state.toml`.
