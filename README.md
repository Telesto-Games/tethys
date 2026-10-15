# tethys
Tethys: a lightweight, LLM-first IDE for Telesto Games' Unreal projects

Open a `.uproject` and Tethys starts Claude Code in the project folder, in a native
GPU-rendered terminal. See [docs/design.md](docs/design.md) for the design.

## Install

Download `tethys-<version>-windows-x64.zip` from [Releases](https://github.com/Telesto-Games/tethys/releases),
unzip it anywhere you can write to, and run `tethys.exe`. It isn't code-signed yet, so Windows
SmartScreen may warn the first time.

Tethys keeps itself up to date: a few seconds after it starts it checks for a newer release and
offers to install it (**Update and restart**, **Skip this version** or **Later**). **Help →
Check for Updates…** checks on demand. Updating replaces `tethys.exe` in place, checked against
the SHA-256 GitHub records for it, then reopens the same project. If the swap fails, the old
`tethys.exe` stays where it was and the download is kept as `tethys.exe.new`. Set
`TETHYS_NO_UPDATE_CHECK=1` to turn the startup check off; debug builds never check by
themselves.

## Build and run

Needs Rust (stable, MSVC) and the Visual Studio C++ build tools.

```sh
cargo run -- path\to\Game.uproject     # or a folder, or just `cargo run` and pick one
cargo build --release                  # target\release\tethys.exe, no console window
```

## Using it

| Do this | How |
|---|---|
| Open a project | Start `tethys.exe` with no argument for the project browser (picker + recent projects), or pass a `.uproject`. Also **Ctrl+Shift+O**, or drop a `.uproject` on the window |
| Open a folder | **File → Open Folder…**, **Open folder…** in the project browser, pass a folder path, or drop a folder on the window. You get agent sessions, terminals and the Files pane; Build, Launch Editor, Debug and Tests need a `.uproject` and are hidden |
| Project browser from a project | **File → Projects…** or **Ctrl+Shift+P** opens it in a new window |
| Menus | **File** (Open Project, Open Folder, Projects, New/Close Session, Settings, Exit), **Edit** (Themes), **Build** (Build, Launch), **Debug**, **Test** (Show Tests, Connect, Start a Test Editor), **Help** (About) |
| Change the look | **Edit → Themes…** picks the theme in use and makes your own: **New theme** copies the selected one, then set its background, text, quiet text and accent colours, font family and size. **Save and use** applies it everywhere at once. Tick **Terminals follow this theme** for sessions and terminals to take its colours and font too. The built-in Telesto theme can't be edited |
| New agent session | **+** at the end of the main area's tab bar (**+ New session** below the sessions with vertical tabs) → Claude Code or opencode; the session opens in that group. **Ctrl+Shift+T** starts the first (Claude Code) |
| Plain terminal | **+** → Terminal, or **Ctrl+Shift+\`**. Runs PowerShell 7 (`pwsh`) if installed, otherwise Windows PowerShell, in the project folder |
| Close session | **Ctrl+Shift+W** |
| Pick the build configuration | The configuration dropdown in the project header, with names as in Visual Studio: **DebugGame / Development / Debug Editor**, then **DebugGame / Development / Debug / Test / Shipping** for the game, and the same for **Client** and **Server** targets when the project has them (remembered between runs). With an installed engine (the Launcher's), Debug, Test, Client and Server are greyed: they need a source-built engine |
| Build | **Ctrl+Shift+B**: runs `Build.bat <Target> Win64 <Configuration>` for the chosen configuration in the **Build pane** on the right (✓ or ✗ when done). Each new build replaces the previous finished log |
| Launch the Unreal Editor, a game or a server | **Ctrl+Shift+E** or **Launch**: starts `UnrealEditor.exe` (or `UnrealEditor-Win64-DebugGame.exe` and so on) on the project, independent of Tethys. **▾** beside it launches as the editor, a **Standalone game** (`-game`), a **Dedicated server** (`-server -log`) or a **Server + client**, and remembers the choice. Only Editor configurations launch; Game, Client and Server configurations are build-only for now (running them needs a cook). Launch, Debug and the test editor use the last Editor configuration chosen |
| Run automation tests | **Tests** in the project header opens the **Tests** pane. **Connect ▾** → **This project's editor** uses the editor you have open, or **Start a test editor (headless)** starts `UnrealEditor-Cmd` just for tests (stopped when you disconnect). Tests show as a tree, the project's own by default (**Project only**). **Run** runs the selected test or group, or every test shown; **Run failed** reruns failures. Select a test for its result and log; click a file name to open the source at that line. Extra test editor arguments are in **File → Settings…** |
| Switch session | **Ctrl+Tab** / **Ctrl+Shift+Tab**, or click the tab |
| Browse files | The **Files** pane on the left lists the project folder; its footer shows the source control in use, Git or Subversion (branch, revision, remote URL, last change). **All / Changes** at its top switches to a flat list of only added and modified files. Changed files are coloured (yellow modified, green added, red deleted/conflicted, grey unversioned) with their SVN-style letter; folders containing changes are yellow. Anything not tracked by source control is italic: `?` untracked, `I` ignored (fainter), including everything inside such a folder |
| Manage files | Right-click in Files: **New File…** / **New Folder…** (in the folder clicked, or the project root below the list), **Rename…**, **Delete…** (asks first, then deletes permanently) and **View History**. Renames and deletes of tracked items go through `svn move` / `git mv` and `svn delete` / `git rm`, so history follows and deletions are ready to commit |
| View a file's history | **View History** opens its commits (following renames) in the main area; click one to see what it changed in that file |
| Edit a file | Click it in Files. Ctrl+S saves, **Revert** discards unsaved edits. Line endings, BOM and tab indentation are preserved exactly. Tabs with unsaved edits can't be closed until saved or reverted |
| Diff a file | Click **diff** next to a changed file in Files, or **Diff** in its editor (includes unsaved edits). Shows a unified diff against SVN BASE or git HEAD |
| Arrange sessions side by side | Drag any tab (sessions, editors, Files, Build, Tests…) onto the edge of a pane to split it, or onto another tab bar to move it. Drag the divider to resize; close a tab with its × |
| Collapse a side pane | The button at the right of a dock's tab bar collapses it to a strip; click the strip, or drop a tab on it, to open it again. Right-click a tab bar to switch between horizontal and vertical tabs |
| Copy | Drag to select, then **Ctrl+C** (or Ctrl+Shift+C) |
| Paste | **Ctrl+V** or right-click |
| Update Tethys | **Help → Check for Updates…** (also checked automatically after startup) |

Every other key goes straight to the agent.

## Releasing

Every merge to `main` ships. `main` is protected, so changes go in through a pull request once
CI (`licenses` and `build`) passes. The **Release** workflow then bumps the latest `v*` tag,
stamps that version into the build, tests, tags the merged commit and publishes a GitHub
Release with `tethys-<version>-windows-x64.zip` (exe, LICENSE, notices, README) and the bare
`tethys.exe` the updater downloads.

Label the PR to choose the bump:

| Label | Bump |
|---|---|
| none | patch (0.4.0 → 0.4.1) |
| `minor` | 0.4.0 → 0.5.0 |
| `major` | 0.4.0 → 1.0.0 |
| `no-release` | merge without releasing; it ships with the next release |

The tags hold the version: `Cargo.toml` isn't bumped, so local builds report whatever it says.
Release notes are generated from the merged PRs; edit them on GitHub afterwards if you like,
since they appear in the update dialog.

To try the updater against the latest release, run an older build or pretend to be one:
`TETHYS_PRETEND_VERSION=0.0.1` (this also enables the startup check in debug builds).

## App icon

Sources are `assets/icon/tethys.svg` and `tethys-small.svg` (16–32 px). After editing them, regenerate the `.ico` with:

```sh
cargo run --manifest-path tools/make-icon/Cargo.toml
```

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

## License

Tethys is MIT licensed: see [LICENSE](LICENSE). Copyright © 2026 Gradient Ascent Ltd.
Third-party code it ships with, and their licenses, are listed in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md). CI runs `cargo deny check licenses` to
keep the dependency tree permissive (see `deny.toml`). Regenerate the notices after changing
dependencies:

```sh
cargo about generate about.hbs -o THIRD-PARTY-NOTICES.md
```

## Trademarks

Telesto® is a registered trademark of Gradient Ascent Ltd. The MIT license covers the code,
not the Telesto name, logo or brand. Unreal® Engine is a trademark of Epic Games, Inc.; Claude
is a trademark of Anthropic, PBC. Tethys is not affiliated with or endorsed by either.
