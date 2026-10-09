//! `LiveCoding` by pressing UE's Live Coding shortcut, Ctrl+Alt+F11, for the
//! user. Both of UE's Live Coding implementations watch the keyboard globally
//! (raw input and `GetAsyncKeyState`), so the editor doesn't need focus and no
//! plugin or setting is needed. A shortcut changed in the editor's Live Coding
//! settings isn't followed.

use tethys_core::ports::{LiveCoding, PortResult};

#[derive(Debug, Default)]
pub struct HotkeyLiveCoding;

impl LiveCoding for HotkeyLiveCoding {
    /// Holds the keys for a moment, so call it off the UI thread.
    fn compile(&self) -> PortResult<()> {
        imp::compile()
    }
}

#[cfg(not(windows))]
mod imp {
    use tethys_core::ports::PortResult;

    pub fn compile() -> PortResult<()> {
        Err("Live Coding is only available on Windows".into())
    }
}

#[cfg(windows)]
mod imp {
    use std::time::Duration;

    use tethys_core::debug;
    use tethys_core::ports::PortResult;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
        VK_CONTROL, VK_F11, VK_MENU,
    };

    /// Ctrl+Alt+F11, pressed in this order and released in reverse.
    const SHORTCUT: [VIRTUAL_KEY; 3] = [VK_CONTROL, VK_MENU, VK_F11];
    /// The older Live Coding server polls the keys every 10 ms.
    const HOLD: Duration = Duration::from_millis(100);

    pub fn compile() -> PortResult<()> {
        if !editor_running()? {
            return Err("no Unreal Editor is running. Launch the editor first".into());
        }
        send(SHORTCUT.iter().map(|&key| keyboard(key, false)))?;
        std::thread::sleep(HOLD);
        send(SHORTCUT.iter().rev().map(|&key| keyboard(key, true)))
    }

    fn keyboard(key: VIRTUAL_KEY, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: key,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn send(inputs: impl Iterator<Item = INPUT>) -> PortResult<()> {
        let inputs: Vec<INPUT> = inputs.collect();
        // SAFETY: `inputs` is a valid array of `len` INPUTs for the call.
        let sent = unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                size_of::<INPUT>() as i32,
            )
        };
        if sent as usize != inputs.len() {
            return Err(format!(
                "Windows blocked the Live Coding shortcut: {}",
                std::io::Error::last_os_error()
            )
            .into());
        }
        Ok(())
    }

    /// Whether any Unreal Editor process is running.
    fn editor_running() -> PortResult<bool> {
        // SAFETY: the snapshot handle is closed below; `entry` is sized as
        // the API requires.
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error().into());
            }
            let mut entry = PROCESSENTRY32W {
                dwSize: size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut found = false;
            let mut more = Process32FirstW(snapshot, &mut entry) != 0;
            while more && !found {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(260);
                found = debug::is_editor_exe(&String::from_utf16_lossy(&entry.szExeFile[..len]));
                more = Process32NextW(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
            Ok(found)
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn lists_processes() {
            // Nothing to assert about which editors run; it mustn't fail.
            super::editor_running().unwrap();
        }

        /// Presses Ctrl+Alt+F11 for real: run with an editor open and check
        /// its log for `LogLiveCoding`. `cargo test -p tethys-adapters
        /// live_coding_compile -- --ignored`
        #[test]
        #[ignore]
        fn live_coding_compile() {
            super::compile().unwrap();
        }
    }
}
