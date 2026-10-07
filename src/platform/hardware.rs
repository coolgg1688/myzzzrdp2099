// Z远程协助(zremote66): one-shot hardware/software/network info collection.
//
// The result is a fixed-shape JSON string (see `collect_config_info`).
// Platform-specific collection lives behind `cfg(target_os = ...)`.
// Every external call / parse is fallible and degrades to an empty string or
// zero; this module must never panic on a controlled peer.
//
// Windows: pure Rust APIs only (winreg + windows crate FFI + windows-service).
//          No std::process::Command on Windows -> no cmd/wmic/powershell popups.
// All GB values are rounded to 1 decimal on the Rust side.
// No new cargo dependencies: only std, serde_json and crates already required.
#![allow(dead_code)]

use serde_json::{json, Value};
use std::collections::HashMap;

const MAX_USERS: usize = 1000;
const MAX_SOFTWARE: usize = 65535;
const MAX_SERVICES: usize = 65535;
const MAX_NET: usize = 16;

/// Round a GB-ish f64 to 1 decimal place.
fn r1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

#[cfg(not(target_os = "windows"))]
fn to_f64(s: &str) -> f64 {
    s.parse().unwrap_or(0.0)
}
#[cfg(not(target_os = "windows"))]
fn to_i64(s: &str) -> i64 {
    s.parse().unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Non-Windows shell helper (Linux/macOS/Android may spawn; no popup issue).
// ---------------------------------------------------------------------------
#[cfg(not(target_os = "windows"))]
fn run(args: &[&str]) -> String {
    use std::process::Command;
    if args.is_empty() {
        return String::new();
    }
    match Command::new(args[0]).args(&args[1..]).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Windows helpers (pure Rust / FFI; NO Command::new anywhere in cfg(windows)).
// ---------------------------------------------------------------------------
#[cfg(windows)]
mod win {
    use super::r1;
    use serde_json::{json, Value};
    use std::collections::{HashSet, HashMap};
    use winreg::enums::*;
    use winreg::RegKey;
    use winreg::reg_key::HKEY;
    // Z远程协助: 用 sysinfo（Rust 最强跨平台硬件库）重构易失败的内存/磁盘/网络采集，
    // 替代脆弱的手写 DeviceIoControl / GetIfTable2 FFI（后者在部分机器 panic 导致配置窗口白屏）。
    use sysinfo::{Disks, Networks, System};

    const DRIVE_FIXED: u32 = 3;

    // ---- registry helpers -------------------------------------------------
    pub fn reg_sz(hive: HKEY, path: &str, value: &str) -> String {
        let k = match RegKey::predef(hive).open_subkey(path) {
            Ok(k) => k,
            Err(_) => return String::new(),
        };
        k.get_value::<String, _>(value).unwrap_or_default()
    }

    // ---- OS / CPU / board / machine via registry --------------------------
    pub fn os_info() -> Value {
        let name = reg_sz(
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
            "ProductName",
        );
        let ver = reg_sz(
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
            "DisplayVersion",
        );
        let arch = std::env::var("PROCESSOR_ARCHITECTURE").unwrap_or_default();
        json!({"name": name, "version": ver, "arch": arch})
    }

    pub fn cpu_info() -> Value {
        let model = reg_sz(
            HKEY_LOCAL_MACHINE,
            r"HARDWARE\DESCRIPTION\System\CentralProcessor\0",
            "ProcessorNameString",
        );
        let freq: i64 = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0")
            .ok()
            .and_then(|k| k.get_value::<u32, _>("~MHz").ok())
            .unwrap_or(0) as i64;
        let threads = num_cpus::get() as i64;
        let cores = num_cpus::get_physical() as i64;
        json!({"model": model, "cores": cores, "threads": threads, "freq_mhz": freq})
    }

    pub fn board_info() -> Value {
        let vendor = reg_sz(
            HKEY_LOCAL_MACHINE,
            r"HARDWARE\DESCRIPTION\System\BIOS",
            "BaseBoardManufacturer",
        );
        let model = reg_sz(
            HKEY_LOCAL_MACHINE,
            r"HARDWARE\DESCRIPTION\System\BIOS",
            "BaseBoardProduct",
        );
        let serial = smbios_board_serial();
        json!({"vendor": vendor, "model": model, "serial": serial})
    }

    pub fn machine_info() -> Value {
        let vendor = reg_sz(
            HKEY_LOCAL_MACHINE,
            r"HARDWARE\DESCRIPTION\System\BIOS",
            "SystemManufacturer",
        );
        let model = reg_sz(
            HKEY_LOCAL_MACHINE,
            r"HARDWARE\DESCRIPTION\System\BIOS",
            "SystemProductName",
        );
        json!({"vendor": vendor, "model": model})
    }

    // ---- GPU via registry -------------------------------------------------
    pub fn gpu() -> String {
        let class_path =
            r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
        let class = match RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(class_path) {
            Ok(k) => k,
            Err(_) => return String::new(),
        };
        let mut seen: HashSet<String> = HashSet::new();
        let mut gpus: Vec<String> = Vec::new();
        for sub in class.enum_keys().take(64) {
            let sub = match sub { Ok(s) => s, Err(_) => continue };
            if !sub.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            if let Ok(k) = class.open_subkey(&sub) {
                if let Ok(d) = k.get_value::<String, _>("DriverDesc") {
                    if !d.is_empty() && seen.insert(d.clone()) {
                        gpus.push(d);
                    }
                }
            }
        }
        gpus.join("; ")
    }

    // ---- Screen via GetDeviceCaps (GDI) -----------------------------------
    pub fn screen() -> String {
        use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC, GetDeviceCaps, HORZRES, VERTRES};
        unsafe {
            let hdc = GetDC(None);
            if hdc.is_invalid() {
                return String::new();
            }
            let w = GetDeviceCaps(Some(hdc), HORZRES) as u32;
            let h = GetDeviceCaps(Some(hdc), VERTRES) as u32;
            let _ = ReleaseDC(None, hdc);
            if w > 0 && h > 0 {
                format!("{}x{}", w, h)
            } else {
                String::new()
            }
        }
    }

    // ---- Memory via sysinfo (robust, no panic) + SMBIOS Type 17 brand -----
    pub fn memory() -> Value {
        let mut sys = System::new_all();
        sys.refresh_memory();
        // Z远程协助: sysinfo 0.33 的 total_memory()/available_memory() 返回字节(bytes)，
        // 直接除以 1048576 得 MB。此前误当 KiB×1024 放大，导致内存漏除 /1024 显示成 KB 值。
        let total_b = sys.total_memory() as f64;
        let avail_b = sys.available_memory() as f64;
        let used_b = (total_b - avail_b).max(0.0);
        let brand = smbios_memory_brand();
        json!({
            "total_mb": r1(total_b / 1048576.0),
            "available_mb": r1(avail_b / 1048576.0),
            "used_mb": r1(used_b / 1048576.0),
            "brand": brand,
        })
    }

    // ---- Disk via sysinfo Disks (name/mount/total/free) ------------------
    pub fn disk() -> Value {
        use sysinfo::Disks;
        let disks = Disks::new_with_refreshed_list();
        let mut total_b = 0f64;
        let mut free_b = 0f64;
        let mut out: Vec<Value> = Vec::new();
        for d in disks.list() {
            let fs = d.file_system().to_string_lossy().into_owned();
            if super::is_virtual_fs(&fs) {
                continue;
            }
            let total = d.total_space() as f64;
            let avail = d.available_space() as f64;
            let used = (total - avail).max(0.0);
            let mount: String = d.mount_point().to_string_lossy().into_owned();
            let dev: String = d.name().to_string_lossy().into_owned();
            total_b += total;
            free_b += avail;
            out.push(json!({
                "name": if mount.is_empty() { dev.clone() } else { mount.clone() },
                "model": dev,
                "total_gb": r1(total / 1073741824.0),
                "free_gb": r1(avail / 1073741824.0),
                "used_gb": r1(used / 1073741824.0),
                "partitions": [{
                    "name": mount,
                    "label": "",
                    "total_gb": r1(total / 1073741824.0),
                    "free_gb": r1(avail / 1073741824.0),
                    "used_gb": r1(used / 1073741824.0),
                }],
            }));
        }
        json!({
            "total_gb": r1(total_b / 1073741824.0),
            "free_gb": r1(free_b / 1073741824.0),
            "used_gb": r1((total_b - free_b).max(0.0) / 1073741824.0),
            "disks": out,
        })
    }

    // Map a volume root (e.g. "C:\") to physical disk number.
    // Returns u32::MAX on failure (caller falls back to a single pseudo-disk).
    fn volume_disk_number(root: windows::core::PCWSTR) -> u32 {
        use windows::Win32::Storage::FileSystem::{
            CreateFileW, OPEN_EXISTING, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::System::IO::DeviceIoControl;
        const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x00560000;
        unsafe {
            let h = match CreateFileW(
                root,
                0u32,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            ) {
                Ok(h) => h,
                Err(_) => return u32::MAX,
            };
            if h.is_invalid() {
                let _ = CloseHandle(h);
                return u32::MAX;
            }
            let mut buf = [0u8; 1024];
            let mut ret = 0u32;
            let ok = DeviceIoControl(
                h,
                IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
                None,
                0,
                Some(buf.as_mut_ptr() as *mut _),
                buf.len() as u32,
                Some(&mut ret),
                None,
            );
            let _ = CloseHandle(h);
            if ok.is_err() {
                return u32::MAX;
            }
            if buf.len() < 4 + 4 {
                return u32::MAX;
            }
            let n = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
            if n == 0 {
                return u32::MAX;
            }
            u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]])
        }
    }

    // Query STORAGE_DEVICE_DESCRIPTOR for \\.\PhysicalDriveN. Returns "Vendor Product" or "".
    fn physical_disk_model(n: u32) -> String {
        use windows::Win32::Storage::FileSystem::{
            CreateFileW, OPEN_EXISTING, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        use windows::Win32::System::Ioctl::{
            STORAGE_PROPERTY_QUERY, StorageDeviceProperty, PropertyStandardQuery,
        };
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::IO::DeviceIoControl;
        const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D1400;
        let path: Vec<u16> = format!("\\\\.\\PhysicalDrive{}\0", n).encode_utf16().collect();
        unsafe {
            let h = match CreateFileW(
                windows::core::PCWSTR(path.as_ptr()),
                0u32,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            ) {
                Ok(h) => h,
                Err(_) => return String::new(),
            };
            if h.is_invalid() {
                let _ = CloseHandle(h);
                return String::new();
            }
            let mut query = STORAGE_PROPERTY_QUERY {
                PropertyId: StorageDeviceProperty,
                QueryType: PropertyStandardQuery,
                AdditionalParameters: [0],
            };
            let mut buf = [0u8; 1024];
            let mut ret = 0u32;
            let ok = DeviceIoControl(
                h,
                IOCTL_STORAGE_QUERY_PROPERTY,
                Some(&mut query as *mut _ as *mut _),
                std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                Some(buf.as_mut_ptr() as *mut _),
                buf.len() as u32,
                Some(&mut ret),
                None,
            );
            let _ = CloseHandle(h);
            if ok.is_err() {
                return String::new();
            }
            if ret < 32 {
                return String::new();
            }
            let vendor_off = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]) as usize;
            let product_off = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]) as usize;
            let read_cstr = |off: usize| -> String {
                if off == 0 || off >= buf.len() {
                    return String::new();
                }
                let end = buf[off..].iter().position(|&c| c == 0).unwrap_or(buf.len() - off);
                String::from_utf8_lossy(&buf[off..off + end]).trim().to_string()
            };
            let vendor = read_cstr(vendor_off);
            let product = read_cstr(product_off);
            if vendor.is_empty() && product.is_empty() {
                String::new()
            } else if vendor.is_empty() {
                product
            } else if product.is_empty() {
                vendor
            } else {
                format!("{} {}", vendor, product)
            }
        }
    }

    // ---- SMBIOS (RSMB) parser for board serial + memory brand -------------
    fn smbios_table() -> Vec<u8> {
        use windows::Win32::System::SystemInformation::{GetSystemFirmwareTable, RSMB};
        unsafe {
            let need = GetSystemFirmwareTable(RSMB, 0, None);
            if need == 0 {
                return Vec::new();
            }
            let mut buf = vec![0u8; need as usize];
            let wrote = GetSystemFirmwareTable(RSMB, 0, Some(buf.as_mut_slice()));
            if wrote == 0 {
                return Vec::new();
            }
            buf.truncate(wrote as usize);
            buf
        }
    }

    fn smbios_board_serial() -> String {
        let buf = smbios_table();
        if buf.len() < 8 {
            return String::new();
        }
        let table_len = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
        let data = &buf[8..std::cmp::min(8 + table_len, buf.len())];
        walk_smbios(data, |stype, hlen, fmt, strings| {
            if stype == 2 && hlen >= 8 {
                return Some(get_str(strings, fmt[7]));
            }
            None
        })
        .unwrap_or_default()
    }

    fn smbios_memory_brand() -> String {
        let buf = smbios_table();
        if buf.len() < 8 {
            return String::new();
        }
        let table_len = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
        let data = &buf[8..std::cmp::min(8 + table_len, buf.len())];
        let mut seen: HashSet<String> = HashSet::new();
        walk_smbios_collect(data, |stype, hlen, fmt, strings| {
            if stype == 17 && hlen >= 25 {
                let mfr = get_str(strings, fmt[21]);
                let pn = get_str(strings, fmt[24]);
                if !mfr.is_empty() || !pn.is_empty() {
                    let combined = if !mfr.is_empty() && !pn.is_empty() {
                        format!("{} {}", mfr, pn)
                    } else if !mfr.is_empty() {
                        mfr
                    } else {
                        pn
                    };
                    seen.insert(combined);
                }
            }
        });
        seen.into_iter().collect::<Vec<_>>().join("; ")
    }

    fn get_str(strings: &[String], idx: u8) -> String {
        if idx == 0 {
            return String::new();
        }
        strings.get((idx - 1) as usize).cloned().unwrap_or_default()
    }

    fn walk_smbios<F: FnMut(u8, usize, &[u8], &[String]) -> Option<String>>(
        data: &[u8],
        mut f: F,
    ) -> Option<String> {
        let mut offset = 0usize;
        while offset + 4 <= data.len() {
            let stype = data[offset];
            let hlen = data[offset + 1] as usize;
            if hlen < 4 || offset + hlen > data.len() {
                break;
            }
            let fmt = &data[offset..offset + hlen];
            let mut p = offset + hlen;
            let mut strings: Vec<String> = Vec::new();
            while p + 1 < data.len() {
                if data[p] == 0 && data[p + 1] == 0 {
                    p += 2;
                    break;
                }
                let mut q = p;
                while q < data.len() && data[q] != 0 {
                    q += 1;
                }
                if q > p {
                    if let Ok(s) = std::str::from_utf8(&data[p..q]) {
                        strings.push(s.to_string());
                    }
                }
                p = q + 1;
            }
            if let Some(s) = f(stype, hlen, fmt, &strings) {
                return Some(s);
            }
            if stype == 127 {
                break;
            }
            offset = p;
        }
        None
    }

    fn walk_smbios_collect<F: FnMut(u8, usize, &[u8], &[String])>(
        data: &[u8],
        mut f: F,
    ) {
        let mut offset = 0usize;
        while offset + 4 <= data.len() {
            let stype = data[offset];
            let hlen = data[offset + 1] as usize;
            if hlen < 4 || offset + hlen > data.len() {
                break;
            }
            let fmt = &data[offset..offset + hlen];
            let mut p = offset + hlen;
            let mut strings: Vec<String> = Vec::new();
            while p + 1 < data.len() {
                if data[p] == 0 && data[p + 1] == 0 {
                    p += 2;
                    break;
                }
                let mut q = p;
                while q < data.len() && data[q] != 0 {
                    q += 1;
                }
                if q > p {
                    if let Ok(s) = std::str::from_utf8(&data[p..q]) {
                        strings.push(s.to_string());
                    }
                }
                p = q + 1;
            }
            f(stype, hlen, fmt, &strings);
            if stype == 127 {
                break;
            }
            offset = p;
        }
    }

    // ---- Users via sysinfo Users (纯 safe，杜绝 NetUserEnum/NetLocalGroupGetMembers
    // 手动指针遍历导致的 access violation——该类崩溃无法被 catch_unwind 捕获) ----
    pub fn users() -> Value {
        use sysinfo::Users;
        let users = Users::new_with_refreshed_list();
        let mut out: Vec<Value> = Vec::new();
        for user in users.list() {
            let name = user.name().to_string();
            if name.is_empty() {
                continue;
            }
            if out.len() >= super::MAX_USERS {
                break;
            }
            out.push(json!({"name": name, "full_name": "", "admin": false}));
        }
        json!(out)
    }

    // ---- Software via winreg enumeration of Uninstall roots ---------------
    pub fn software() -> Value {
        let roots = [
            (HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"),
            (
                HKEY_LOCAL_MACHINE,
                r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
            (HKEY_CURRENT_USER, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"),
        ];
        let mut out: Vec<Value> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for (hive, path) in roots.iter() {
            let root = match RegKey::predef(*hive).open_subkey(path) {
                Ok(k) => k,
                Err(_) => continue,
            };
            for sub in root.enum_keys() {
                let sub = match sub { Ok(s) => s, Err(_) => continue };
                if out.len() >= super::MAX_SOFTWARE {
                    break;
                }
                let k = match root.open_subkey(&sub) {
                    Ok(k) => k,
                    Err(_) => continue,
                };
                let name: String = match k.get_value("DisplayName") {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if name.is_empty() {
                    continue;
                }
                let ver: String = k.get_value("DisplayVersion").unwrap_or_default();
                let pub_: String = k.get_value("Publisher").unwrap_or_default();
                let key = format!("{}\u{1}{}", name, ver);
                if !seen.insert(key) {
                    continue;
                }
                out.push(json!({"name": name, "version": ver, "publisher": pub_}));
            }
        }
        json!(out)
    }

    // ---- Services via raw Windows API (EnumServicesStatusExW) --------------
    pub fn services() -> Value {
        use windows::Win32::System::Services::{
            OpenSCManagerW, EnumServicesStatusExW, CloseServiceHandle,
            SC_MANAGER_ENUMERATE_SERVICE, SC_MANAGER_CONNECT,
            SC_ENUM_PROCESS_INFO, SERVICE_WIN32, SERVICE_STATE_ALL,
            SERVICE_RUNNING, SERVICE_STOPPED,
            OpenServiceW, QueryServiceConfigW, SERVICE_QUERY_CONFIG,
            ENUM_SERVICE_STATUS_PROCESSW,
        };
        use windows::core::PCWSTR;

        unsafe {
            let mgr = match OpenSCManagerW(
                PCWSTR::null(),
                PCWSTR::null(),
                SC_MANAGER_ENUMERATE_SERVICE | SC_MANAGER_CONNECT,
            ) {
                Ok(h) => h,
                Err(_) => return json!(Vec::<Value>::new()),
            };

            // First call: get required buffer size.
            let mut needed = 0u32;
            let mut returned = 0u32;
            let mut resume = 0u32;
            let _ = EnumServicesStatusExW(
                mgr,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                None,
                &mut needed,
                &mut returned,
                Some(&mut resume),
                PCWSTR::null(),
            );

            if needed == 0 {
                let _ = CloseServiceHandle(mgr);
                return json!(Vec::<Value>::new());
            }

            let mut buf = vec![0u8; needed as usize];
            let ok = EnumServicesStatusExW(
                mgr,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                Some(buf.as_mut_slice()),
                &mut needed,
                &mut returned,
                Some(&mut resume),
                PCWSTR::null(),
            );

            if ok.is_err() || returned == 0 {
                let _ = CloseServiceHandle(mgr);
                return json!(Vec::<Value>::new());
            }

            let mut out: Vec<Value> = Vec::new();
            let entry_size = std::mem::size_of::<ENUM_SERVICE_STATUS_PROCESSW>();
            // 防御：以实际缓冲容量为上限，杜绝枚举数量超过缓冲区导致的越界读。
            let max_entries = if entry_size == 0 { 0 } else { buf.len() / entry_size };
            for i in 0..(returned as usize).min(max_entries) {
                if out.len() >= super::MAX_SERVICES {
                    break;
                }
                let ptr = buf.as_ptr().add(i * entry_size) as *const ENUM_SERVICE_STATUS_PROCESSW;
                let entry = &*ptr;
                let name = PCWSTR(entry.lpServiceName.0).to_string().unwrap_or_default();
                let state = entry.ServiceStatusProcess.dwCurrentState;
                let status = if state == SERVICE_RUNNING {
                    "running"
                } else if state == SERVICE_STOPPED {
                    "stopped"
                } else {
                    ""
                };

                // Try to get start type via QueryServiceConfigW.
                let start_type: String = {
                    let svc = OpenServiceW(
                        mgr,
                        PCWSTR(entry.lpServiceName.0),
                        SERVICE_QUERY_CONFIG,
                    );
                    match svc {
                        Ok(sh) => {
                            let mut cfg_buf = [0u8; 1024];
                            let mut cb_needed = 0u32;
                            let r = QueryServiceConfigW(
                                sh,
                                None,
                                0,
                                &mut cb_needed,
                            );
                            let result = if r.is_err() && cb_needed > 0 && cb_needed <= cfg_buf.len() as u32 {
                                // Retry with proper buffer.
                                let mut cfg_buf2 = vec![0u8; cb_needed as usize];
                                let r2 = QueryServiceConfigW(
                                    sh,
                                    Some(cfg_buf2.as_mut_ptr() as *mut _),
                                    cfg_buf2.len() as u32,
                                    &mut cb_needed,
                                );
                                if r2.is_ok() {
                                    let cfg = &*(cfg_buf2.as_ptr() as *const windows::Win32::System::Services::QUERY_SERVICE_CONFIGW);
                                    match cfg.dwStartType.0 {
                                        0 => "boot",
                                        1 => "system",
                                        2 => "auto",
                                        3 => "manual",
                                        4 => "disabled",
                                        _ => "",
                                    }.to_string()
                                } else {
                                    String::new()
                                }
                            } else {
                                String::new()
                            };
                            let _ = CloseServiceHandle(sh);
                            result
                        }
                        Err(_) => String::new(),
                    }
                };

                out.push(json!({"name": name, "status": status, "start_type": start_type}));
            }
            let _ = CloseServiceHandle(mgr);
            json!(out)
        }
    }

    // ---- Network adapters + throughput via sysinfo (no panic-prone FFI) ---
    pub fn net() -> Value {
        use sysinfo::Networks;
        let mut networks = Networks::new_with_refreshed_list();
        let _ = networks.refresh(false);
        std::thread::sleep(std::time::Duration::from_millis(500));
        let _ = networks.refresh(false);
        let mut out: Vec<Value> = Vec::new();
        for (name, data) in networks.list() {
            // received()/transmitted() = bytes since last refresh (over the 0.5s window).
            let rx_kbps = r1(data.received() as f64 / 0.5 / 1024.0);
            let tx_kbps = r1(data.transmitted() as f64 / 0.5 / 1024.0);
            out.push(json!({
                "name": name.clone(),
                "mac": "",
                "ip": "",
                "rx_kbps": rx_kbps,
                "tx_kbps": tx_kbps,
            }));
        }
        json!(out)
    }

    fn sample_if_octets() -> HashMap<u32, (u64, u64)> {
        use windows::Win32::NetworkManagement::IpHelper::{GetIfTable2, FreeMibTable, MIB_IF_TABLE2};
        use windows::Win32::Foundation::WIN32_ERROR;
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        let st = unsafe { GetIfTable2(&mut table) };
        let mut map = HashMap::new();
        if st != WIN32_ERROR(0) || table.is_null() {
            return map;
        }
        unsafe {
            let num = (*table).NumEntries as usize;
            let rows = (*table).Table.as_ptr();
            for i in 0..num {
                let r = &*rows.add(i);
                map.insert(r.InterfaceIndex, (r.InOctets, r.OutOctets));
            }
            FreeMibTable(table as *const _);
        }
        map
    }
}

// ---------------------------------------------------------------------------
// Windows collect()
// ---------------------------------------------------------------------------
#[cfg(windows)]
fn collect() -> Value {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use windows::Win32::System::SystemInformation::GetTickCount64;
    let uptime_s = unsafe { GetTickCount64() } as f64 / 1000.0;
    // Z远程协助: 每个采集项独立 catch_unwind，避免任一项 panic 拖垮整个被控端进程
    // （安卓获取 win 配置时 win 自动退出即此类崩溃）。
    json!({
        "os": catch_unwind(AssertUnwindSafe(win::os_info)).unwrap_or_else(|_| json!({})),
        "cpu": catch_unwind(AssertUnwindSafe(win::cpu_info)).unwrap_or_else(|_| json!({})),
        "memory": catch_unwind(AssertUnwindSafe(win::memory)).unwrap_or_else(|_| json!({})),
        "disk": catch_unwind(AssertUnwindSafe(win::disk)).unwrap_or_else(|_| json!({})),
        "gpu": catch_unwind(AssertUnwindSafe(win::gpu)).unwrap_or_else(|_| String::new()),
        "board": catch_unwind(AssertUnwindSafe(win::board_info)).unwrap_or_else(|_| json!({})),
        "machine": catch_unwind(AssertUnwindSafe(win::machine_info)).unwrap_or_else(|_| json!({})),
        "screen": catch_unwind(AssertUnwindSafe(win::screen)).unwrap_or_else(|_| String::new()),
        "uptime": format_up(uptime_s),
        "users": catch_unwind(AssertUnwindSafe(win::users)).unwrap_or_else(|_| json!([])),
        "software": catch_unwind(AssertUnwindSafe(win::software)).unwrap_or_else(|_| json!([])),
        "services": catch_unwind(AssertUnwindSafe(win::services)).unwrap_or_else(|_| json!([])),
        "net": catch_unwind(AssertUnwindSafe(win::net)).unwrap_or_else(|_| json!([])),
    })
}

// ---------------------------------------------------------------------------
// Linux
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
fn collect() -> Value {
    let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let mut os_name = String::new();
    let mut os_ver = String::new();
    for line in os_release.lines() {
        if let Some(rest) = line.strip_prefix("NAME=") {
            os_name = rest.trim_matches('"').to_string();
        } else if let Some(rest) = line.strip_prefix("VERSION=") {
            os_ver = rest.trim_matches('"').to_string();
        }
    }
    let arch = run(&["uname", "-m"]);
    let os = json!({"name": os_name, "version": os_ver, "arch": arch});

    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let mut cpu_model = String::new();
    let mut threads: i64 = 0;
    let mut freq: f64 = 0.0;
    for line in cpuinfo.lines() {
        if let Some(rest) = line.strip_prefix("model name") {
            if let Some(eq) = rest.find(':') {
                if cpu_model.is_empty() {
                    cpu_model = rest[eq + 1..].trim().to_string();
                }
            }
        } else if line.starts_with("processor") {
            threads += 1;
        } else if let Some(rest) = line.strip_prefix("cpu MHz") {
            if let Some(eq) = rest.find(':') {
                if freq == 0.0 {
                    freq = to_f64(rest[eq + 1..].trim());
                }
            }
        }
    }
    let cores = num_cpus::get_physical() as i64;
    let cpu = json!({"model": cpu_model, "cores": cores, "threads": threads, "freq_mhz": freq as i64});

    let memory = sys_memory();
    let disk = sys_disks();

    let gpu = run(&["sh", "-c", "lspci -mm 2>/dev/null | grep -iE 'vga|3d|display' | cut -d'\"' -f2 | paste -sd '; ' -"]);

    let board_vendor = std::fs::read_to_string("/sys/class/dmi/id/board_vendor").unwrap_or_default().trim().to_string();
    let board_model = std::fs::read_to_string("/sys/class/dmi/id/board_name").unwrap_or_default().trim().to_string();
    let board_serial = std::fs::read_to_string("/sys/class/dmi/id/board_serial").unwrap_or_default().trim().to_string();
    let board = json!({"vendor": board_vendor, "model": board_model, "serial": board_serial});

    let sys_vendor = std::fs::read_to_string("/sys/class/dmi/id/sys_vendor").unwrap_or_default().trim().to_string();
    let product_name = std::fs::read_to_string("/sys/class/dmi/id/product_name").unwrap_or_default().trim().to_string();
    let machine = json!({"vendor": sys_vendor, "model": product_name});

    let uptime_raw = std::fs::read_to_string("/proc/uptime").unwrap_or_default();
    let uptime = uptime_raw.split_whitespace().next().map(|s| format_up(to_f64(s))).unwrap_or_default();

    let group = std::fs::read_to_string("/etc/group").unwrap_or_default();
    let mut admin_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in group.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() >= 4 && (f[0] == "sudo" || f[0] == "wheel" || f[0] == "admin") {
            for u in f[3].split(',') {
                let u = u.trim();
                if !u.is_empty() { admin_set.insert(u.to_string()); }
            }
        }
    }
    let mut users: Vec<Value> = Vec::new();
    if let Ok(passwd) = std::fs::read_to_string("/etc/passwd") {
        for line in passwd.lines() {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() >= 7 {
                let shell = f[6];
                if !shell.contains("nologin") && !shell.contains("/false") && users.len() < MAX_USERS {
                    let is_admin = f[1] == "0" || f[0] == "root" || admin_set.contains(f[0]);
                    users.push(json!({"name": f[0], "full_name": f[4].split(',').next().unwrap_or(""), "admin": is_admin}));
                }
            }
        }
    }

    let mut software: Vec<Value> = Vec::new();
    let dpkg = run(&["sh", "-c", "dpkg-query -W --showformat='${Package}\\t${Version}\\n' 2>/dev/null"]);
    for line in dpkg.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() >= 2 && software.len() < MAX_SOFTWARE {
            software.push(json!({"name": f[0], "version": f[1], "publisher": ""}));
        }
    }
    if software.is_empty() {
        let rpm = run(&["sh", "-c", "rpm -qa --qf '%{NAME}\\t%{VERSION}\\n' 2>/dev/null"]);
        for line in rpm.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() >= 2 && software.len() < MAX_SOFTWARE {
                software.push(json!({"name": f[0], "version": f[1], "publisher": ""}));
            }
        }
    }

    let mut services: Vec<Value> = Vec::new();
    let sc = run(&["sh", "-c", "systemctl list-units --type=service --all --no-legend --no-pager 2>/dev/null"]);
    for line in sc.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() >= 3 && f[0] != "UNIT" && services.len() < MAX_SERVICES {
            services.push(json!({"name": f[0], "status": f[2], "start_type": ""}));
        }
    }

    let net = sys_networks();

    json!({
        "os": os, "cpu": cpu, "memory": memory, "disk": disk,
        "gpu": gpu, "board": board, "machine": machine, "screen": "", "uptime": uptime,
        "users": users, "software": software, "services": services, "net": net,
    })
}

#[cfg(target_os = "linux")]
fn collect_linux_net() -> Value {
    use std::collections::HashMap;
    let mut ifaces: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/sys/class/net") {
        for e in rd.flatten() {
            if let Some(n) = e.file_name().to_str() {
                ifaces.push(n.to_string());
            }
        }
    }
    let mut ip_map: HashMap<String, String> = HashMap::new();
    let ipout = run(&["sh", "-c", "ip -4 -o addr show 2>/dev/null"]);
    for line in ipout.lines() {
        let p: Vec<&str> = line.split_whitespace().collect();
        if p.len() >= 4 && p[2] == "inet" {
            let iface = p[1];
            let addr = p[3].split('/').next().unwrap_or("");
            ip_map.insert(iface.to_string(), addr.to_string());
        }
    }
    fn sample_dev() -> HashMap<String, (u64, u64)> {
        let mut m = HashMap::new();
        let s = std::fs::read_to_string("/proc/net/dev").unwrap_or_default();
        for line in s.lines() {
            let line = line.trim();
            if !line.contains(':') { continue; }
            let (name, rest) = line.split_once(':').unwrap_or(("", ""));
            let p: Vec<&str> = rest.split_whitespace().collect();
            if p.len() >= 9 {
                let rx: u64 = p[0].parse().unwrap_or(0);
                let tx: u64 = p[8].parse().unwrap_or(0);
                m.insert(name.trim().to_string(), (rx, tx));
            }
        }
        m
    }
    let s1 = sample_dev();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let s2 = sample_dev();
    let mut out: Vec<Value> = Vec::new();
    for iface in ifaces {
        if out.len() >= MAX_NET { break; }
        let mac = std::fs::read_to_string(format!("/sys/class/net/{}/address", iface))
            .unwrap_or_default().trim().to_string();
        let ip = ip_map.get(&iface).cloned().unwrap_or_default();
        let (rx, tx) = match (s1.get(&iface), s2.get(&iface)) {
            (Some(a), Some(b)) => {
                let drx = b.0.saturating_sub(a.0) as f64;
                let dtx = b.1.saturating_sub(a.1) as f64;
                (r1(drx/0.5/1024.0), r1(dtx/0.5/1024.0))
            }
            _ => (0.0, 0.0),
        };
        out.push(json!({"name": iface, "mac": mac, "ip": ip, "rx_kbps": rx, "tx_kbps": tx}));
    }
    json!(out)
}

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------
#[cfg(target_os = "macos")]
fn collect() -> Value {
    let os_name = run(&["sw_vers", "-productName"]);
    let os_ver = run(&["sw_vers", "-productVersion"]);
    let arch = run(&["uname", "-m"]);
    let os = json!({"name": os_name, "version": os_ver, "arch": arch});

    let cpu_model = run(&["sysctl", "-n", "machdep.cpu.brand_string"]);
    let physical: i64 = run(&["sysctl", "-n", "hw.physicalcpu"]).parse().unwrap_or(0);
    let logical: i64 = run(&["sysctl", "-n", "hw.logicalcpu"]).parse().unwrap_or(0);
    let freq: i64 = run(&["sysctl", "-n", "hw.cpufrequency"]).parse().unwrap_or(0) / 1_000_000;
    let cpu = json!({"model": cpu_model, "cores": physical, "threads": logical, "freq_mhz": freq});

    let memory = sys_memory();
    let disk = sys_disks();

    let gpu = run(&["sh", "-c", "system_profiler SPDisplaysDataType 2>/dev/null | grep 'Chipset Model' | cut -d: -f2 | paste -sd '; ' -"]);
    let board_model = run(&["sysctl", "-n", "hw.model"]);
    let board_serial = run(&["sh", "-c", "ioreg -l 2>/dev/null | awk -F'\"' '/IOPlatformSerialNumber/ {print $4; exit}'"]);
    let board = json!({"vendor": "Apple", "model": board_model, "serial": board_serial});
    let machine = json!({"vendor": "Apple", "model": board_model});

    let uptime = {
        let boot_raw = run(&["sysctl", "-n", "kern.boottime"]);
        let now_raw = run(&["date", "+%s"]);
        let mut boot_sec: f64 = 0.0;
        if let Some(idx) = boot_raw.find("sec") {
            let tail = &boot_raw[idx + 3..];
            let digits: String = tail.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
            boot_sec = to_f64(&digits);
        }
        let now_sec = to_f64(now_raw.trim());
        if boot_sec > 0.0 && now_sec > boot_sec {
            format_up(now_sec - boot_sec)
        } else {
            String::new()
        }
    };

    let admin_members = run(&["dscl", ".", "-read", "/Groups/admin", "GroupMembership"]);
    let mut admin_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    for tok in admin_members.split_whitespace() {
        if tok != "GroupMembership:" { admin_set.insert(tok.to_string()); }
    }
    let mut users: Vec<Value> = Vec::new();
    let dscl = run(&["dscl", ".", "list", "/Users"]);
    for line in dscl.lines() {
        let u = line.trim();
        if !u.is_empty() && !u.starts_with('_') && u != "daemon" && u != "Guest" && users.len() < MAX_USERS {
            let is_admin = admin_set.contains(u);
            users.push(json!({"name": u, "full_name": "", "admin": is_admin}));
        }
    }

    let mut software: Vec<Value> = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/Applications") {
        for e in rd.flatten() {
            if let Some(n) = e.file_name().to_str() {
                if n.ends_with(".app") && software.len() < MAX_SOFTWARE {
                    software.push(json!({"name": n.trim_end_matches(".app"), "version": "", "publisher": ""}));
                }
            }
        }
    }

    let mut services: Vec<Value> = Vec::new();
    let lc = run(&["launchctl", "list"]);
    for line in lc.lines() {
        let p: Vec<&str> = line.split_whitespace().collect();
        if p.len() >= 3 && p[0] != "PID" && services.len() < MAX_SERVICES {
            let status = if p[1] == "0" { "running" } else { "stopped" };
            services.push(json!({"name": p[2], "status": status, "start_type": ""}));
        }
    }

    let net = sys_networks();

    json!({
        "os": os, "cpu": cpu, "memory": memory, "disk": disk,
        "gpu": gpu, "board": board, "machine": machine, "screen": "", "uptime": uptime,
        "users": users, "software": software, "services": services, "net": net,
    })
}

#[cfg(target_os = "macos")]
fn collect_macos_net() -> Value {
    use std::collections::HashMap;
    let ifconfig = run(&["ifconfig"]);
    let mut ifaces: Vec<(String, String, String)> = Vec::new();
    let mut cur: Option<&str> = None;
    for line in ifconfig.lines() {
        if !line.starts_with('\t') && line.contains(':') {
            let name = line.split(':').next().unwrap_or("").to_string();
            if !name.is_empty() { cur = Some(Box::leak(name.into_boxed_str())); }
        } else if let Some(n) = cur {
            let t = line.trim();
            if t.starts_with("ether ") {
                ifaces.push((n.to_string(), t.split_whitespace().nth(1).unwrap_or("").to_string(), String::new()));
            } else if t.starts_with("inet ") {
                if let Some(last) = ifaces.last_mut() {
                    if last.2.is_empty() {
                        last.2 = t.split_whitespace().nth(1).unwrap_or("").to_string();
                    }
                }
            }
        }
    }
    fn sample_netstat() -> HashMap<String, (u64, u64)> {
        let mut m = HashMap::new();
        let out = run(&["netstat", "-ib"]);
        for line in out.lines().skip(1) {
            let p: Vec<&str> = line.split_whitespace().collect();
            if p.len() >= 10 {
                let rx: u64 = p[6].parse().unwrap_or(0);
                let tx: u64 = p[7].parse().unwrap_or(0);
                m.entry(p[0].to_string()).or_insert((0, 0));
                let e = m.get_mut(p[0]).unwrap();
                e.0 += rx; e.1 += tx;
            }
        }
        m
    }
    let s1 = sample_netstat();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let s2 = sample_netstat();
    let mut out: Vec<Value> = Vec::new();
    for (name, mac, ip) in ifaces {
        if out.len() >= MAX_NET { break; }
        let (rx, tx) = match (s1.get(&name), s2.get(&name)) {
            (Some(a), Some(b)) => {
                let drx = b.0.saturating_sub(a.0) as f64;
                let dtx = b.1.saturating_sub(a.1) as f64;
                (r1(drx/0.5/1024.0), r1(dtx/0.5/1024.0))
            }
            _ => (0.0, 0.0),
        };
        out.push(json!({"name": name, "mac": mac, "ip": ip, "rx_kbps": rx, "tx_kbps": tx}));
    }
    json!(out)
}

// ---------------------------------------------------------------------------
// Android
// ---------------------------------------------------------------------------
#[cfg(target_os = "android")]
fn collect() -> Value {
    let prop = |k: &str| run(&["getprop", k]);
    let os = json!({
        "name": format!("Android {}", prop("ro.build.version.release")),
        "version": prop("ro.build.version.release"),
        "arch": run(&["uname", "-m"]),
    });
    let manufacturer = prop("ro.product.manufacturer");
    let model = prop("ro.product.model");

    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let mut cpu_model = String::new();
    let mut threads: i64 = 0;
    for line in cpuinfo.lines() {
        if line.starts_with("Processor") || line.starts_with("Hardware") {
            if let Some(eq) = line.find(':') {
                if cpu_model.is_empty() {
                    cpu_model = line[eq + 1..].trim().to_string();
                }
            }
        } else if line.starts_with("processor") {
            threads += 1;
        }
    }
    let cpu = json!({"model": cpu_model, "cores": 0, "threads": threads, "freq_mhz": 0});

    let memory = sys_memory();
    let disk = sys_disks();

    let serial = prop("ro.serialno");
    let board = json!({"vendor": manufacturer, "model": model, "serial": serial});
    let machine = json!({"vendor": manufacturer, "model": model});

    let uptime_raw = std::fs::read_to_string("/proc/uptime").unwrap_or_default();
    let uptime = uptime_raw.split_whitespace().next().map(|s| format_up(to_f64(s))).unwrap_or_default();

    let mut users: Vec<Value> = Vec::new();
    let pu = run(&["pm", "list", "users"]);
    for line in pu.lines() {
        if line.trim_end().starts_with(' ') && line.contains(':') {
            let body = line.trim();
            if let Some(colon) = body.find(':') {
                let name = body[colon + 1..].split_whitespace().next().unwrap_or("");
                if !name.is_empty() && users.len() < MAX_USERS {
                    users.push(json!({"name": name, "full_name": "", "admin": false}));
                }
            }
        }
    }

    let mut software: Vec<Value> = Vec::new();
    let pkgs = run(&["pm", "list", "packages"]);
    for line in pkgs.lines() {
        let l = line.trim();
        if l.starts_with("package:") && software.len() < MAX_SOFTWARE {
            software.push(json!({"name": l.trim_start_matches("package:"), "version": "", "publisher": ""}));
        }
    }

    let services: Vec<Value> = Vec::new();
    let net = sys_networks();

    json!({
        "os": os, "cpu": cpu, "memory": memory, "disk": disk,
        "gpu": "", "board": board, "machine": machine, "screen": "", "uptime": uptime,
        "users": users, "software": software, "services": services, "net": net,
    })
}

#[cfg(target_os = "android")]
fn collect_android_net() -> Value {
    use std::collections::HashMap;
    let mut ifaces: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/sys/class/net") {
        for e in rd.flatten() {
            if let Some(n) = e.file_name().to_str() {
                ifaces.push(n.to_string());
            }
        }
    }
    fn sample_dev() -> HashMap<String, (u64, u64)> {
        let mut m = HashMap::new();
        let s = std::fs::read_to_string("/proc/net/dev").unwrap_or_default();
        for line in s.lines() {
            let line = line.trim();
            if !line.contains(':') { continue; }
            let (name, rest) = line.split_once(':').unwrap_or(("", ""));
            let p: Vec<&str> = rest.split_whitespace().collect();
            if p.len() >= 9 {
                let rx: u64 = p[0].parse().unwrap_or(0);
                let tx: u64 = p[8].parse().unwrap_or(0);
                m.insert(name.trim().to_string(), (rx, tx));
            }
        }
        m
    }
    let s1 = sample_dev();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let s2 = sample_dev();
    let mut out: Vec<Value> = Vec::new();
    for iface in ifaces {
        if out.len() >= MAX_NET { break; }
        let mac = std::fs::read_to_string(format!("/sys/class/net/{}/address", iface)).unwrap_or_default().trim().to_string();
        let (rx, tx) = match (s1.get(&iface), s2.get(&iface)) {
            (Some(a), Some(b)) => {
                let drx = b.0.saturating_sub(a.0) as f64;
                let dtx = b.1.saturating_sub(a.1) as f64;
                (r1(drx/0.5/1024.0), r1(dtx/0.5/1024.0))
            }
            _ => (0.0, 0.0),
        };
        out.push(json!({"name": iface, "mac": mac, "ip": "", "rx_kbps": rx, "tx_kbps": tx}));
    }
    json!(out)
}

// ---------------------------------------------------------------------------
// Fallback
// ---------------------------------------------------------------------------
#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos", target_os = "android")))]
fn collect() -> Value {
    json!({
        "os": {"name":"","version":"","arch":""},
        "cpu": {"model":"","cores":0,"threads":0,"freq_mhz":0},
        "memory": {"total_gb":0.0,"available_gb":0.0,"used_gb":0.0,"brand":""},
        "disk": {"total_gb":0.0,"free_gb":0.0,"used_gb":0.0,"disks":[]},
        "gpu": "",
        "board": {"vendor":"","model":"","serial":""},
        "machine": {"vendor":"","model":""},
        "screen": "", "uptime": "",
        "users": [], "software": [], "services": [], "net": [],
    })
}

// Z远程协助: 跨平台共享 sysinfo 采集（windows/linux/macos/android 统一），
// 替代各平台脆弱的手写 /proc、命令、FFI 实现，避免采集 panic 导致配置窗口白屏。
// Android: sysinfo's total_memory() returns bytes (not KiB) on this target,
// which was being multiplied again by 1024 and produced absurd "GB" values.
// Read /proc/meminfo directly (kB) for a correct MB figure.
#[cfg(target_os = "android")]
fn meminfo_kb(key: &str) -> f64 {
    let s = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(eq) = rest.find(':') {
                let v: f64 = rest[eq + 1..]
                    .trim()
                    .trim_end_matches("kB")
                    .trim()
                    .parse()
                    .unwrap_or(0.0);
                return v;
            }
        }
    }
    0.0
}

#[cfg(target_os = "android")]
fn sys_memory() -> Value {
    let total_kb = meminfo_kb("MemTotal");
    let mut avail_kb = meminfo_kb("MemAvailable");
    if avail_kb <= 0.0 {
        avail_kb = meminfo_kb("MemFree");
    }
    let total_mb = total_kb / 1024.0;
    let avail_mb = avail_kb / 1024.0;
    json!({
        "total_mb": r1(total_mb),
        "available_mb": r1(avail_mb),
        "used_mb": r1((total_mb - avail_mb).max(0.0)),
        "brand": "",
    })
}

#[cfg(not(target_os = "android"))]
fn sys_memory() -> Value {
    use sysinfo::System;
    let mut sys = System::new_all();
    sys.refresh_memory();
    // Z远程协助: sysinfo 0.33 total_memory()/available_memory() 返回字节(bytes)，
    // 直接 /1048576 得 MB，此前误当 KiB×1024 放大导致内存漏除 /1024。
    let total_b = sys.total_memory() as f64;
    let avail_b = sys.available_memory() as f64;
    json!({
        "total_mb": r1(total_b / 1048576.0),
        "available_mb": r1(avail_b / 1048576.0),
        "used_mb": r1((total_b - avail_b).max(0.0) / 1048576.0),
        "brand": "",
    })
}

// Virtual/pseudo filesystems that sysinfo exposes as "disks" but are not real
// physical storage (the cause of "硬盘列出很多 / 与实际硬盘数不符").
fn is_virtual_fs(fs: &str) -> bool {
    let fs = fs.to_lowercase();
    matches!(
        fs.as_str(),
        "tmpfs" | "devtmpfs" | "proc" | "procfs" | "sysfs" | "devpts" | "overlay"
            | "overlayfs" | "shm" | "mqueue" | "cgroup" | "cgroup2" | "pstore" | "bpf"
            | "tracefs" | "debugfs" | "securityfs" | "configfs" | "binfmt_misc"
            | "fusectl" | "autofs" | "hugetlbfs" | "nsfs" | "ramfs" | "rpc_pipefs"
            | "selinuxfs" | "squashfs" | "nfsd"
    )
}

/// Z远程协助: 安卓真实物理存储挂载点白名单。sysinfo 在安卓把 /data 下每个应用
/// 子目录(/data/data/*、/data/user/*、/data/misc/* 等 200+ 项)都列为独立挂载点；
/// 这里只认内部存储与外部 SD 卡：/storage/*、/sdcard 以及根目录 /。其余伪挂载点
/// 全部跳过，从而"硬盘数量=实际物理存储数"。
#[cfg(target_os = "android")]
fn is_real_storage(mount: &str) -> bool {
    mount == "/"
        || mount.starts_with("/storage/")
        || mount == "/sdcard"
        || mount.starts_with("/sdcard/")
}

fn sys_disks() -> Value {
    use sysinfo::Disks;
    let disks = Disks::new_with_refreshed_list();
    let mut total_b = 0f64;
    let mut free_b = 0f64;
    let mut out: Vec<Value> = Vec::new();
    for d in disks.list() {
        let fs = d.file_system().to_string_lossy().into_owned();
        if is_virtual_fs(&fs) {
            continue;
        }
        let mount: String = d.mount_point().to_string_lossy().into_owned();
        // Z远程协助: 安卓再按挂载点路径过滤，只保留真实物理存储卷(见 is_real_storage)。
        #[cfg(target_os = "android")]
        if !is_real_storage(&mount) {
            continue;
        }
        let total = d.total_space() as f64;
        let avail = d.available_space() as f64;
        let used = (total - avail).max(0.0);
        let dev: String = d.name().to_string_lossy().into_owned();
        total_b += total;
        free_b += avail;
        out.push(json!({
            "name": if mount.is_empty() { dev.clone() } else { mount.clone() },
            "model": dev,
            "total_gb": r1(total / 1073741824.0),
            "free_gb": r1(avail / 1073741824.0),
            "used_gb": r1(used / 1073741824.0),
            "partitions": [json!({
                "name": mount,
                "label": "",
                "total_gb": r1(total / 1073741824.0),
                "free_gb": r1(avail / 1073741824.0),
                "used_gb": r1(used / 1073741824.0),
            })],
        }));
    }
    json!({
        "total_gb": r1(total_b / 1073741824.0),
        "free_gb": r1(free_b / 1073741824.0),
        "used_gb": r1((total_b - free_b).max(0.0) / 1073741824.0),
        "disks": out,
    })
}

fn sys_networks() -> Value {
    use sysinfo::Networks;
    let mut networks = Networks::new_with_refreshed_list();
    let _ = networks.refresh(false);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let _ = networks.refresh(false);
    let mut out: Vec<Value> = Vec::new();
    for (name, data) in networks.list() {
        out.push(json!({
            "name": name.clone(),
            "mac": "",
            "ip": "",
            "rx_kbps": r1(data.received() as f64 / 0.5 / 1024.0),
            "tx_kbps": r1(data.transmitted() as f64 / 0.5 / 1024.0),
        }));
    }
    json!(out)
}

fn format_up(secs: f64) -> String {
    let total = secs as u64;
    let d = total / 86400;
    let h = (total % 86400) / 3600;
    let m = (total % 3600) / 60;
    if d > 0 {
        format!("{}d:{}h:{}m", d, h, m)
    } else if h > 0 {
        format!("{}h:{}m", h, m)
    } else {
        format!("{}m", m)
    }
}

/// Collect the local hardware/software/network configuration and serialize it to
/// the fixed-shape JSON string transported back to the controller.
///
/// Note: on all platforms, `net` throughput fields perform a 500ms blocking
/// sample (two reads of InOctets/OutOctets separated by 500ms). This call runs
/// synchronously inside a tokio worker; the 500ms cap keeps the blocking cost
/// bounded.
// Z远程协助: 采集 计算机名 / 本地IP / 互联网IP。全部带兜底，任何一步失败只返回空串，绝不崩溃。
// - hostname: 平台环境变量/文件，纯 std。
// - local_ip: UDP connect 技巧(不发送数据)，返回路由到公网的本地接口 IP，纯 std。
// - public_ip: 尽力查询外部 IP 服务(短超时+catch_unwind)，失败留空。
fn net_identity() -> (String, String, String) {
    let hostname = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .or_else(|_| {
            // env::var 的 or_else 闭包需返回 Result<String, VarError>，不能返回 Option。
            std::fs::read_to_string("/etc/hostname")
                .map(|s| s.trim().to_string())
                .map_err(|_| std::env::VarError::NotPresent)
        })
        .unwrap_or_default();

    let mut local_ip = String::new();
    if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if sock.connect("8.8.8.8:80").is_ok() {
            if let Ok(a) = sock.local_addr() {
                local_ip = a.ip().to_string();
            }
        }
    }

    let mut public_ip = String::new();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        use std::time::Duration;
        let c = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(3))
            .build();
        let Ok(c) = c else { return String::new() };
        let Ok(r) = c.get("https://api.ipify.org").send() else {
            return String::new()
        };
        r.text().map(|t| t.trim().to_string()).unwrap_or_default()
    }));
    if let Ok(ip) = r {
        public_ip = ip;
    }

    (hostname, local_ip, public_ip)
}

pub fn collect_config_info() -> String {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    // Z远程协助: 顶层 catch_unwind 兜底——即使 collect() 内部某处出现未预期 panic，
    // 也只会返回兜底 JSON，绝不拖垮整个被控端进程(修复"安卓获取 win 配置时 win 退出")。
    let mut data = catch_unwind(AssertUnwindSafe(collect)).unwrap_or_else(|_| {
        json!({"os": {}, "cpu": {}, "memory": {}, "disk": {}, "error": "采集失败"})
    });
    // Z远程协助: 附加 计算机名/本地IP/互联网IP(供前端「操作系统」下方展示)。
    if let Some(map) = data.as_object_mut() {
        let (hn, li, pi) = net_identity();
        map.insert("hostname".to_owned(), json!(hn));
        map.insert("local_ip".to_owned(), json!(li));
        map.insert("public_ip".to_owned(), json!(pi));
    }
    match serde_json::to_string(&data) {
        Ok(s) => s,
        Err(_) => String::new(),
    }
}
