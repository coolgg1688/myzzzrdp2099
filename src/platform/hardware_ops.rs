// Z远程协助(zremote66): config-info operation pipeline.
//
// Receives an op command delivered through the existing config-info MessageBox channel
// ({"op":"uninstall|service_stop|service_start|refresh","name":"...","param":"..."})
// and applies it with each platform's native APIs. Returns a result JSON string
// ({"ok":bool,"message":"...","data":<optional>}). The module must never panic; every
// external call is fallible and degrades to an error result.
//
// Windows: pure native APIs only (winreg + windows crate FFI + windows-service).
//          No std::process::Command -> no cmd/console popup. Uninstall uses
//          CreateProcessW(CREATE_NO_WINDOW) when elevated, else ShellExecuteW("runas")
//          to request a native UAC elevation.
// Linux:   pkexec (polkit native auth dialog) around dpkg/rpm/systemctl.
// macOS:   osascript 'do shell script ... with administrator privileges' (native auth).
// Android: not supported (system-level permissions required).

use serde_json::{json, Value};

/// Entry point. `json` is the request payload from the controller. Returns the result
/// payload (JSON string) to ship back as a `zremote66-config-op-result` MessageBox.
pub fn execute_op(json: &str) -> String {
    let v: Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return fail_result("无效的操作请求".to_owned()),
    };
    let op = v.get("op").and_then(|s| s.as_str()).unwrap_or("");
    let name = v
        .get("name")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_owned();
    match op {
        "refresh" => refresh(),
        "uninstall" => uninstall(&name),
        "service_stop" => service_control(&name, false),
        "service_start" => service_control(&name, true),
        _ => fail_result(format!("未知操作: {op}")),
    }
}

fn ok_result(message: String) -> String {
    json!({"ok": true, "message": message}).to_string()
}

pub fn fail_result(message: String) -> String {
    json!({"ok": false, "message": message}).to_string()
}

/// Re-run the existing config-info collection and ship the parsed data back.
fn refresh() -> String {
    let collected = crate::platform::hardware::collect_config_info();
    match serde_json::from_str::<Value>(&collected) {
        Ok(data) => json!({"ok": true, "message": "已刷新", "data": data}).to_string(),
        Err(_) => json!({"ok": true, "message": "已刷新", "data": Value::Null}).to_string(),
    }
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------
#[cfg(windows)]
#[allow(dead_code)]
mod win_impl {
    use super::{fail_result, ok_result};

    pub fn uninstall(name: &str) -> String {
        use winreg::enums::*;
        use winreg::RegKey;
        let roots = [
            (HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"),
            (
                HKEY_LOCAL_MACHINE,
                r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
            (
                HKEY_CURRENT_USER,
                r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
        ];
        let mut cmdline: Option<String> = None;
        for (hive, path) in roots.iter() {
            let root = match RegKey::predef(*hive).open_subkey(path) {
                Ok(k) => k,
                Err(_) => continue,
            };
            for sub in root.enum_keys().flatten() {
                let k = match root.open_subkey(&sub) {
                    Ok(k) => k,
                    Err(_) => continue,
                };
                let dn: String = match k.get_value("DisplayName") {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if dn == name {
                    // Prefer the already-silent uninstall string; otherwise use the
                    // normal one and append a heuristic silent flag below.
                    let quiet: String = k.get_value("QuietUninstallString").unwrap_or_default();
                    let normal: String = k.get_value("UninstallString").unwrap_or_default();
                    if !quiet.is_empty() {
                        cmdline = Some(quiet);
                    } else if !normal.is_empty() {
                        cmdline = Some(append_silent_flag(&normal));
                    }
                    break;
                }
            }
            if cmdline.is_some() {
                break;
            }
        }
        let cmd = match cmdline {
            Some(c) if !c.is_empty() => c,
            _ => return fail_result("未找到该软件".to_owned()),
        };
        // Already elevated (service runs as SYSTEM) -> hidden process, no popup.
        // Otherwise request a native UAC elevation via ShellExecuteW("runas").
        if is_elevated() {
            match spawn_hidden(&cmd) {
                Ok(_) => ok_result("已启动卸载".to_owned()),
                Err(e) => fail_result(e),
            }
        } else {
            match shell_execute_runas(&cmd) {
                Ok(_) => ok_result("已启动卸载".to_owned()),
                Err(e) => fail_result(e),
            }
        }
    }

    fn append_silent_flag(s: &str) -> String {
        let lower = s.to_lowercase();
        if lower.contains("msiexec") {
            if lower.contains("/qn") || lower.contains("/quiet") || lower.contains("/passive") {
                s.to_owned()
            } else {
                format!("{s} /qn")
            }
        } else if lower.contains("/s ")
            || lower.contains("/silent")
            || lower.contains("/verysilent")
            || lower.contains("/quiet")
            || lower.ends_with("/s")
        {
            s.to_owned()
        } else {
            format!("{s} /S")
        }
    }

    pub fn service_control(name: &str, start: bool) -> String {
        use std::ffi::OsStr;
        use windows_service::service::{Service, ServiceAccess};
        use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
        let access = if start {
            ServiceAccess::START
        } else {
            ServiceAccess::STOP
        };
        let mgr: ServiceManager =
            match ServiceManager::local_computer(None::<&OsStr>, ServiceManagerAccess::CONNECT) {
                Ok(m) => m,
                Err(e) => return err_from_svc(e),
            };
        let svc: Service = match mgr.open_service(name, access) {
            Ok(s) => s,
            Err(e) => return err_from_svc(e),
        };
        let r = if start {
            svc.start(&[] as &[&OsStr])
        } else {
            svc.stop().map(|_| ())
        };
        match r {
            Ok(_) => ok_result(if start {
                "已启动服务".to_owned()
            } else {
                "已停止服务".to_owned()
            }),
            Err(e) => err_from_svc(e),
        }
    }

    fn err_from_svc(e: windows_service::Error) -> String {
        match &e {
            windows_service::Error::Winapi(ioe) => {
                if ioe.raw_os_error() == Some(5) {
                    fail_result("权限不足，请以管理员身份运行Z远程协助被控端".to_owned())
                } else {
                    fail_result(format!("服务操作失败: {e}"))
                }
            }
            _ => fail_result(format!("服务操作失败: {e}")),
        }
    }

    fn is_elevated() -> bool {
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::Security::{
            GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
        };
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        unsafe {
            let mut token = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                return false;
            }
            let mut elevation = TOKEN_ELEVATION::default();
            let mut returned = 0u32;
            let r = GetTokenInformation(
                token,
                TokenElevation,
                Some(&mut elevation as *mut _ as *mut core::ffi::c_void),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut returned,
            );
            let _ = CloseHandle(token);
            r.is_ok() && elevation.TokenIsElevated != 0
        }
    }

    // Split a command line into (program, arguments) for ShellExecuteW.
    fn split_command(cmdline: &str) -> (String, String) {
        let cmdline = cmdline.trim();
        if let Some(rest) = cmdline.strip_prefix('"') {
            if let Some(end) = rest.find('"') {
                let file = rest[..end].to_owned();
                let params = rest[end + 1..].trim_start().to_owned();
                return (file, params);
            }
        }
        match cmdline.find(|c: char| c.is_whitespace()) {
            Some(i) => (
                cmdline[..i].to_owned(),
                cmdline[i + 1..].trim_start().to_owned(),
            ),
            None => (cmdline.to_owned(), String::new()),
        }
    }

    // Elevated path: hidden process, no console window.
    fn spawn_hidden(command_line: &str) -> Result<(), String> {
        use windows::core::{PCWSTR, PWSTR};
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            CreateProcessW, CREATE_NO_WINDOW, PROCESS_INFORMATION, STARTUPINFOW,
        };
        let mut cmd: Vec<u16> = command_line.encode_utf16().chain(Some(0)).collect();
        let mut si = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut pi = PROCESS_INFORMATION::default();
        unsafe {
            CreateProcessW(
                PCWSTR::null(),
                Some(PWSTR(cmd.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_NO_WINDOW,
                None,
                PCWSTR::null(),
                &mut si,
                &mut pi,
            )
            .map_err(|e| format!("CreateProcessW 失败: {e}"))?;
            let _ = CloseHandle(pi.hProcess);
            let _ = CloseHandle(pi.hThread);
        }
        Ok(())
    }

    // Non-elevated path: request a native UAC prompt via the "runas" verb.
    fn shell_execute_runas(cmdline: &str) -> Result<(), String> {
        use windows::core::PCWSTR;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let (file, params) = split_command(cmdline);
        let op: Vec<u16> = "runas\0".encode_utf16().collect();
        let file_w: Vec<u16> = file.encode_utf16().chain(Some(0)).collect();
        let params_w: Vec<u16> = params.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let r = ShellExecuteW(
                None,
                PCWSTR(op.as_ptr()),
                PCWSTR(file_w.as_ptr()),
                PCWSTR(params_w.as_ptr()),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
            // ShellExecuteW returns a HINSTANCE whose low value <= 32 is an error code.
            if r.0 as isize <= 32 {
                return Err(format!("ShellExecuteW 失败，错误代码 {}", r.0 as isize));
            }
            Ok(())
        }
    }
}

#[cfg(windows)]
fn uninstall(name: &str) -> String {
    win_impl::uninstall(name)
}

#[cfg(windows)]
fn service_control(name: &str, start: bool) -> String {
    win_impl::service_control(name, start)
}

// ---------------------------------------------------------------------------
// Linux
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
fn uninstall(name: &str) -> String {
    use std::process::Command;
    let dpkg = Command::new("pkexec").args(["dpkg", "-r", name]).output();
    match dpkg {
        Ok(o) if o.status.success() => ok_result("已卸载".to_owned()),
        _ => {
            let rpm = Command::new("pkexec").args(["rpm", "-e", name]).output();
            match rpm {
                Ok(o) if o.status.success() => ok_result("已卸载".to_owned()),
                Ok(o) => fail_result(format!(
                    "卸载失败: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                )),
                Err(e) => fail_result(format!("卸载失败: {e}")),
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn service_control(name: &str, start: bool) -> String {
    use std::process::Command;
    let action = if start { "start" } else { "stop" };
    let msg = if start { "已启动服务" } else { "已停止服务" };
    match Command::new("pkexec")
        .args(["systemctl", action, name])
        .output()
    {
        Ok(o) if o.status.success() => ok_result(msg.to_owned()),
        Ok(o) => fail_result(format!(
            "服务操作失败: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => fail_result(format!("服务操作失败: {e}")),
    }
}

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------
#[cfg(target_os = "macos")]
fn uninstall(name: &str) -> String {
    let base = name.strip_suffix(".app").unwrap_or(name);
    let script = format!("rm -rf \"/Applications/{base}.app\"");
    run_admin_osascript(&script)
}

#[cfg(target_os = "macos")]
fn service_control(name: &str, start: bool) -> String {
    // launchctl bootout/bootstrap needs the full plist path, which we do not have from
    // a bare service name. Fall back to the per-domain start/stop verbs (best effort).
    let verb = if start { "start" } else { "stop" };
    let script = format!("launchctl {verb} {name}");
    run_admin_osascript(&script)
}

#[cfg(target_os = "macos")]
fn run_admin_osascript(script: &str) -> String {
    use std::process::Command;
    let full = format!("do shell script \"{script}\" with administrator privileges");
    match Command::new("osascript").args(["-e", &full]).output() {
        Ok(o) if o.status.success() => ok_result("操作完成".to_owned()),
        Ok(o) => fail_result(format!(
            "操作失败: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => fail_result(format!("操作失败: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Android
// ---------------------------------------------------------------------------
#[cfg(target_os = "android")]
fn uninstall(name: &str) -> String {
    use std::process::Command;
    match Command::new("pm").args(["uninstall", name]).output() {
        Ok(o) if o.status.success() => ok_result("已卸载".to_owned()),
        _ => fail_result("Android 卸载需要系统级权限，暂不支持".to_owned()),
    }
}

#[cfg(target_os = "android")]
fn service_control(_name: &str, _start: bool) -> String {
    fail_result("暂不支持".to_owned())
}

// ---------------------------------------------------------------------------
// Unknown / unsupported platforms
// ---------------------------------------------------------------------------
#[cfg(not(any(windows, target_os = "linux", target_os = "macos", target_os = "android")))]
fn uninstall(_name: &str) -> String {
    fail_result("暂不支持".to_owned())
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos", target_os = "android")))]
fn service_control(_name: &str, _start: bool) -> String {
    fail_result("暂不支持".to_owned())
}
