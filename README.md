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
the SHA-256 GitHub records for it, then reopens the same project. Set
`TETHYS_NO_UPDATE_CHECK=1` to turn the startup check off; debug builds never check by
themselves.

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
| Plain terminal | **New session ▾** → Terminal, or **Ctrl+Shift+\`**. Runs PowerShell 7 (`pwsh`) if installed, otherwise Windows PowerShell, in the project folder |
| Close session | **Ctrl+Shift+W** |
| Pick the build configuration | **Development** / **DebugGame** toggle in the project header (remembered between runs) |
| Build the editor target | **Ctrl+Shift+B**: runs `Build.bat <Project>Editor Win64 <Configuration>` in the **Build pane** on the right (✓ or ✗ when done). Each new build replaces the previous finished log |
| Launch the Unreal Editor | **Ctrl+Shift+E**: starts `UnrealEditor.exe` (or `UnrealEditor-Win64-DebugGame.exe`) on the project, independent of Tethys |
| Switch session | **Ctrl+Tab** / **Ctrl+Shift+Tab**, or click the tab |
| Browse files | The **Files** pane on the left lists the project folder; its footer shows the source control in use, Git or Subversion (branch, revision, remote URL, last change). **All / Changes** at its top switches to a flat list of only added and modified files. Changed files are coloured (yellow modified, green added, red deleted/conflicted, grey unversioned) with their SVN-style letter; folders containing changes are yellow |
| Edit a file | Click it in Files. Ctrl+S saves, **Revert** discards unsaved edits. Line endings, BOM and tab indentation are preserved exactly. Tabs with unsaved edits can't be closed until saved or reverted |
| Diff a file | Click **diff** next to a changed file in Files, or **Diff** in its editor (includes unsaved edits). Shows a unified diff against SVN BASE or git HEAD |
| Arrange sessions side by side | Drag a tab onto the edge of a pane to split it, or onto another tab bar to move it. Drag the divider to resize; close a tab with its × |
| Copy | Drag to select, then **Ctrl+C** (or Ctrl+Shift+C) |
| Paste | **Ctrl+V** or right-click |
| Update Tethys | **Help → Check for Updates…** (also checked automatically after startup) |

Every other key goes straight to the agent.

## Releasing

`main` is protected: every change, including a version bump, goes in through a pull request
once CI (`licenses` and `build`) passes.

1. Bump `version` under `[workspace.package]` in `Cargo.toml` in a PR ("Release 0.2.0") and
   merge it.
2. Tag the merged commit and push the tag:
   `git checkout main && git pull && git tag v0.2.0 && git push origin v0.2.0`.

The **Release** workflow checks the tag matches the version, builds, and publishes a GitHub
Release with `tethys-0.2.0-windows-x64.zip` (exe, LICENSE, notices, README) and the bare
`tethys.exe` the updater downloads. Release notes are generated from the commits; edit them on
GitHub afterwards if you like, since they appear in the update dialog.

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
