// Z远程协助(zremote66): one-shot hardware/software info collection.
//
// The result is a fixed-shape JSON string (see `collect_config_info`).
// Platform-specific collection lives behind `cfg(target_os = ...)`.
// Every external call / parse is fallible and degrades to an empty string or
// zero; this module must never panic on a controlled peer.
//
// No new cargo dependencies: only std and serde_json (already required).
#![allow(dead_code)]

use serde_json::{json, Value};
use std::process::Command;

const MAX_USERS: usize = 100;
const MAX_SOFTWARE: usize = 500;
const MAX_SERVICES: usize = 300;

/// Run a process, trim stdout, return "" on any failure (no panic).
#[cfg(not(target_os = "windows"))]
fn run(args: &[&str]) -> String {
    if args.is_empty() {
        return String::new();
    }
    match Command::new(args[0]).args(&args[1..]).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => String::new(),
    }
}

#[cfg(windows)]
fn run(args: &[&str]) -> String {
    if args.is_empty() {
        return String::new();
    }
    // Go through cmd so builtins and relative names resolve the same way everywhere.
    match Command::new("cmd")
        .arg("/C")
        .args(args)
        .output()
    {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => String::new(),
    }
}

/// Read a `reg query KEY /v VALUE` string (REG_SZ) value; "" if absent.
#[cfg(windows)]
fn reg_sz(key: &str, value: &str) -> String {
    let out = run(&["reg", "query", key, "/v", value]);
    // Lines look like:
    //    ProductName    REG_SZ    Windows 10 Pro
    for line in out.lines() {
        let line = line.trim();
        if line.starts_with(value) {
            if let Some(idx) = line.find("REG_SZ") {
                return line[idx + "REG_SZ".len()..].trim().to_string();
            }
        }
    }
    String::new()
}

/// Parse `KEY=VALUE` style `/value` output (wmic) into a map.
fn parse_value_lines(out: &str) -> Vec<(String, String)> {
    let mut v = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(eq) = line.find('=') {
            let k = line[..eq].trim().to_string();
            let val = line[eq + 1..].trim().to_string();
            v.push((k, val));
        }
    }
    v
}

fn to_f64(s: &str) -> f64 {
    s.parse().unwrap_or(0.0)
}
fn to_i64(s: &str) -> i64 {
    s.parse().unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------
#[cfg(windows)]
fn collect() -> Value {
    let arch = std::env::var("PROCESSOR_ARCHITECTURE").unwrap_or_default();

    // OS
    let os_name = reg_sz("HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion", "ProductName");
    let os_ver = reg_sz("HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion", "DisplayVersion");
    let os = json!({"name": os_name, "version": os_ver, "arch": arch});

    // CPU (registry + wmic fallback)
    let cpu_model = reg_sz(
        "HKLM\\HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0",
        "ProcessorNameString",
    );
    let threads_env: i64 = std::env::var("NUMBER_OF_PROCESSORS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let wmic_cpu = run(&["wmic", "cpu", "get", "NumberOfCores,NumberOfLogicalProcessors,MaxClockSpeed", "/value"]);
    let mut cores = 0i64;
    let mut threads = threads_env;
    let mut freq = 0i64;
    for (k, v) in parse_value_lines(&wmic_cpu) {
        match k.as_str() {
            "NumberOfCores" => cores = to_i64(&v),
            "NumberOfLogicalProcessors" => threads = to_i64(&v),
            "MaxClockSpeed" => freq = to_i64(&v),
            _ => {}
        }
    }
    let cpu = json!({"model": cpu_model, "cores": cores, "threads": threads, "freq_mhz": freq});

    // Memory (KB from wmic OS)
    let mem = run(&["wmic", "OS", "get", "TotalVisibleMemorySize,FreePhysicalMemory", "/value"]);
    let mut total_kb: f64 = 0.0;
    let mut free_kb: f64 = 0.0;
    for (k, v) in parse_value_lines(&mem) {
        match k.as_str() {
            "TotalVisibleMemorySize" => total_kb = to_f64(&v),
            "FreePhysicalMemory" => free_kb = to_f64(&v),
            _ => {}
        }
    }
    let memory = json!({
        "total_gb": (total_kb / 1024.0 / 1024.0) as f64,
        "available_gb": (free_kb / 1024.0 / 1024.0) as f64,
    });

    // Disk: sum fixed drives (bytes)
    let disk_out = run(&["wmic", "logicaldisk", "where", "DriveType=3", "get", "Size,FreeSpace", "/value"]);
    let mut total_b: f64 = 0.0;
    let mut free_b: f64 = 0.0;
    let mut cur_size: f64 = 0.0;
    for (k, v) in parse_value_lines(&disk_out) {
        match k.as_str() {
            "Size" => cur_size = to_f64(&v),
            "FreeSpace" => {
                total_b += cur_size;
                free_b += to_f64(&v);
            }
            _ => {}
        }
    }
    let disk = json!({
        "total_gb": total_b / 1024.0 / 1024.0 / 1024.0,
        "free_gb": free_b / 1024.0 / 1024.0 / 1024.0,
    });

    // GPU names
    let gpu_out = run(&["wmic", "path", "win32_VideoController", "get", "Name,CurrentHorizontalResolution,CurrentVerticalResolution", "/value"]);
    let mut gpus: Vec<String> = Vec::new();
    let mut screen = String::new();
    let mut cur_w: i64 = 0;
    let mut cur_h: i64 = 0;
    for (k, v) in parse_value_lines(&gpu_out) {
        match k.as_str() {
            "Name" => {
                if !v.is_empty() {
                    gpus.push(v);
                }
            }
            "CurrentHorizontalResolution" => cur_w = to_i64(&v),
            "CurrentVerticalResolution" => {
                cur_h = to_i64(&v);
                if cur_w > 0 && cur_h > 0 && screen.is_empty() {
                    screen = format!("{}x{}", cur_w, cur_h);
                }
            }
            _ => {}
        }
    }
    let gpu = gpus.join("; ");

    // Board
    let board_out = run(&["wmic", "baseboard", "get", "Manufacturer,Product", "/value"]);
    let mut board_vendor = String::new();
    let mut board_model = String::new();
    for (k, v) in parse_value_lines(&board_out) {
        match k.as_str() {
            "Manufacturer" => board_vendor = v,
            "Product" => board_model = v,
            _ => {}
        }
    }
    let board = json!({"vendor": board_vendor, "model": board_model});

    // Uptime as an elapsed-duration string (not a boot timestamp).
    // [Environment]::TickCount64/1000 gives seconds since boot.
    let tick = run(&[
        "powershell",
        "-NoProfile",
        "-Command",
        "[math]::Floor([Environment]::TickCount64/1000)",
    ]);
    let uptime = format_up(to_f64(tick.trim()));

    // Users
    let admins = run(&["net", "localgroup", "administrators"]);
    let mut admin_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in admins.lines() {
        let t = line.trim();
        // Skip header/footer lines; group members are plain usernames.
        if t.is_empty() || t.starts_with("Member") || t.starts_with("---")
            || t.starts_with("The command")
        {
            continue;
        }
        admin_set.insert(t.to_lowercase());
    }
    let users_out = run(&["wmic", "useraccount", "get", "Name,FullName", "/value"]);
    let mut users: Vec<Value> = Vec::new();
    let mut uname = String::new();
    for (k, v) in parse_value_lines(&users_out) {
        match k.as_str() {
            "Name" => uname = v,
            "FullName" => {
                if !uname.is_empty() && users.len() < MAX_USERS {
                    let is_admin = admin_set.contains(&uname.to_lowercase());
                    users.push(json!({"name": uname, "full_name": v, "admin": is_admin}));
                    uname = String::new();
                }
            }
            _ => {}
        }
    }

    // Software: registry uninstall keys (DisplayName / DisplayVersion / Publisher)
    let software = collect_windows_software();

    // Services
    let svc_out = run(&["wmic", "service", "get", "Name,State,StartMode", "/value"]);
    let mut services: Vec<Value> = Vec::new();
    let mut sname = String::new();
    let mut sstate = String::new();
    for (k, v) in parse_value_lines(&svc_out) {
        match k.as_str() {
            "Name" => sname = v,
            "State" => sstate = v,
            "StartMode" => {
                if !sname.is_empty() && services.len() < MAX_SERVICES {
                    services.push(json!({"name": sname, "status": sstate, "start_type": v}));
                    sname = String::new();
                    sstate = String::new();
                }
            }
            _ => {}
        }
    }

    json!({
        "os": os, "cpu": cpu, "memory": memory, "disk": disk,
        "gpu": gpu, "board": board, "screen": screen, "uptime": uptime,
        "users": users, "software": software, "services": services,
    })
}

#[cfg(windows)]
fn collect_windows_software() -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for key in &[
        "HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
        "HKLM\\SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
    ] {
        let dump = run(&["reg", "query", key]);
        let mut sub = String::new();
        let mut name = String::new();
        let mut ver = String::new();
        let mut pub_ = String::new();
        // The /s dump interleaves subkey headers with value lines; walk lines and
        // emit one record per DisplayName seen.
        for line in dump.lines() {
            let t = line.trim();
            if t.starts_with("HKEY") {
                // new subkey: flush previous
                if !name.is_empty() && out.len() < MAX_SOFTWARE {
                    out.push(json!({"name": name, "version": ver, "publisher": pub_}));
                }
                sub = t.to_string();
                name = String::new();
                ver = String::new();
                pub_ = String::new();
                continue;
            }
            if t.starts_with("DisplayName") {
                if let Some(i) = t.find("REG_SZ") {
                    name = t[i + "REG_SZ".len()..].trim().to_string();
                }
            } else if t.starts_with("DisplayVersion") {
                if let Some(i) = t.find("REG_SZ") {
                    ver = t[i + "REG_SZ".len()..].trim().to_string();
                }
            } else if t.starts_with("Publisher") {
                if let Some(i) = t.find("REG_SZ") {
                    pub_ = t[i + "REG_SZ".len()..].trim().to_string();
                }
            }
        }
        if !name.is_empty() && out.len() < MAX_SOFTWARE {
            out.push(json!({"name": name, "version": ver, "publisher": pub_}));
        }
        let _ = sub;
    }
    out
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
    let cpu = json!({"model": cpu_model, "cores": 0, "threads": threads, "freq_mhz": freq as i64});

    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let mut total_kb: f64 = 0.0;
    let mut avail_kb: f64 = 0.0;
    for line in meminfo.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = to_f64(rest.replace("kB", "").trim());
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail_kb = to_f64(rest.replace("kB", "").trim());
        }
    }
    let memory = json!({"total_gb": total_kb/1024.0/1024.0, "available_gb": avail_kb/1024.0/1024.0});

    let df = run(&["df", "-B1", "--total"]);
    let mut disk_total: f64 = 0.0;
    let mut disk_free: f64 = 0.0;
    for line in df.lines() {
        if line.starts_with("total") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                disk_total = to_f64(parts[1]);
                disk_free = to_f64(parts[3]);
            }
        }
    }
    let disk = json!({"total_gb": disk_total/1024.0/1024.0/1024.0, "free_gb": disk_free/1024.0/1024.0/1024.0});

    let gpu = run(&["sh", "-c", "lspci -mm 2>/dev/null | grep -iE 'vga|3d|display' | cut -d'\"' -f2 | paste -sd '; ' -"]);

    let board_vendor = std::fs::read_to_string("/sys/class/dmi/id/board_vendor").unwrap_or_default().trim().to_string();
    let board_model = std::fs::read_to_string("/sys/class/dmi/id/board_name").unwrap_or_default().trim().to_string();
    let board = json!({"vendor": board_vendor, "model": board_model});

    let uptime_raw = std::fs::read_to_string("/proc/uptime").unwrap_or_default();
    let mut uptime = String::new();
    if let Some(first) = uptime_raw.split_whitespace().next() {
        let secs: f64 = to_f64(first);
        uptime = format_up(secs);
    }

    // Users from /etc/passwd; admin = uid 0 or member of sudo/wheel/admin group.
    let group = std::fs::read_to_string("/etc/group").unwrap_or_default();
    let mut admin_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in group.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() >= 4 && (f[0] == "sudo" || f[0] == "wheel" || f[0] == "admin") {
            for u in f[3].split(',') {
                let u = u.trim();
                if !u.is_empty() {
                    admin_set.insert(u.to_string());
                }
            }
        }
    }
    let mut users: Vec<Value> = Vec::new();
    if let Ok(passwd) = std::fs::read_to_string("/etc/passwd") {
        for line in passwd.lines() {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() >= 7 {
                let shell = f[6];
                // only real users (nologin shells skipped)
                if !shell.contains("nologin") && !shell.contains("/false") && users.len() < MAX_USERS {
                    let is_admin = f[1] == "0" || f[0] == "root" || admin_set.contains(f[0]);
                    users.push(json!({"name": f[0], "full_name": f[4].split(',').next().unwrap_or(""), "admin": is_admin}));
                }
            }
        }
    }

    // Software: dpkg
    let mut software: Vec<Value> = Vec::new();
    let dpkg = run(&["sh", "-c", "dpkg-query -W --showformat='${Package}\t${Version}\n' 2>/dev/null"]);
    for line in dpkg.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() >= 2 && software.len() < MAX_SOFTWARE {
            software.push(json!({"name": f[0], "version": f[1], "publisher": ""}));
        }
    }

    // Services: systemctl
    let mut services: Vec<Value> = Vec::new();
    let sc = run(&["sh", "-c", "systemctl list-units --type=service --all --no-legend --no-pager 2>/dev/null"]);
    for line in sc.lines() {
        // Columns: UNIT LOAD ACTIVE SUB DESCRIPTION -> status = ACTIVE (f[2]).
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() >= 3 && f[0] != "UNIT" && services.len() < MAX_SERVICES {
            services.push(json!({"name": f[0], "status": f[2], "start_type": ""}));
        }
    }

    json!({
        "os": os, "cpu": cpu, "memory": memory, "disk": disk,
        "gpu": gpu, "board": board, "screen": "", "uptime": uptime,
        "users": users, "software": software, "services": services,
    })
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

    let mem_bytes: f64 = run(&["sysctl", "-n", "hw.memsize"]).parse().unwrap_or(0.0);
    let memory = json!({"total_gb": mem_bytes/1024.0/1024.0/1024.0, "available_gb": 0.0});

    let df = run(&["df", "-k", "/"]);
    let mut disk_total: f64 = 0.0;
    let mut disk_free: f64 = 0.0;
    for line in df.lines() {
        if line.contains("/dev/disk") {
            let p: Vec<&str> = line.split_whitespace().collect();
            if p.len() >= 4 {
                disk_total = to_f64(p[1]) * 1024.0;
                disk_free = to_f64(p[3]) * 1024.0;
            }
        }
    }
    let disk = json!({"total_gb": disk_total/1024.0/1024.0/1024.0, "free_gb": disk_free/1024.0/1024.0/1024.0});

    let gpu = run(&["sh", "-c", "system_profiler SPDisplaysDataType 2>/dev/null | grep 'Chipset Model' | cut -d: -f2 | paste -sd '; ' -"]);
    let board_model = run(&["sysctl", "-n", "hw.model"]);
    let board = json!({"vendor": "Apple", "model": board_model});

    // Uptime = now - boottime, as an elapsed-duration string (not a timestamp).
    // sysctl -n kern.boottime -> "{ sec = 123456, usec = 789 }".
    let uptime = {
        let boot_raw = run(&["sysctl", "-n", "kern.boottime"]);
        let now_raw = run(&["date", "+%s"]);
        // Extract the integer following "sec" from the boottime struct.
        let mut boot_sec: f64 = 0.0;
        if let Some(idx) = boot_raw.find("sec") {
            let tail = &boot_raw[idx + 3..];
            let digits: String = tail
                .chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit())
                .collect();
            boot_sec = to_f64(&digits);
        }
        let now_sec = to_f64(now_raw.trim());
        if boot_sec > 0.0 && now_sec > boot_sec {
            format_up(now_sec - boot_sec)
        } else {
            String::new()
        }
    };

    // Users; admin = member of the local "admin" group.
    let admin_members = run(&["dscl", ".", "-read", "/Groups/admin", "GroupMembership"]);
    let mut admin_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    for tok in admin_members.split_whitespace() {
        // Output: "GroupMembership: user1 user2 ..."
        if tok != "GroupMembership:" {
            admin_set.insert(tok.to_string());
        }
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

    // Software: /Applications
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

    // Services: launchctl list — columns are PID Status Label.
    let mut services: Vec<Value> = Vec::new();
    let lc = run(&["launchctl", "list"]);
    for line in lc.lines() {
        let p: Vec<&str> = line.split_whitespace().collect();
        if p.len() >= 3 && p[0] != "PID" && services.len() < MAX_SERVICES {
            // p[0]=PID, p[1]=last exit status / 0 when running, p[2]=Label.
            let status = if p[1] == "0" { "running" } else { "stopped" };
            services.push(json!({"name": p[2], "status": status, "start_type": ""}));
        }
    }

    json!({
        "os": os, "cpu": cpu, "memory": memory, "disk": disk,
        "gpu": gpu, "board": board, "screen": "", "uptime": uptime,
        "users": users, "software": software, "services": services,
    })
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

    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let mut total_kb: f64 = 0.0;
    let mut avail_kb: f64 = 0.0;
    for line in meminfo.lines() {
        if let Some(r) = line.strip_prefix("MemTotal:") {
            total_kb = to_f64(r.replace("kB", "").trim());
        } else if let Some(r) = line.strip_prefix("MemAvailable:") {
            avail_kb = to_f64(r.replace("kB", "").trim());
        }
    }
    let memory = json!({"total_gb": total_kb/1024.0/1024.0, "available_gb": avail_kb/1024.0/1024.0});

    // df -B1 columns: Filesystem 1B-blocks Used Available Use% Mounted.
    let df = run(&["df", "-B1", "/data"]);
    let mut disk_total: f64 = 0.0;
    let mut disk_free: f64 = 0.0;
    for line in df.lines() {
        let p: Vec<&str> = line.split_whitespace().collect();
        if p.len() >= 6 && (p[5].trim() == "/" || p[5].trim() == "/data") {
            disk_total = to_f64(p[1]);
            disk_free = to_f64(p[3]);
        }
    }
    let disk = json!({"total_gb": disk_total/1024.0/1024.0/1024.0, "free_gb": disk_free/1024.0/1024.0/1024.0});

    let board = json!({"vendor": manufacturer, "model": model});

    let uptime_raw = std::fs::read_to_string("/proc/uptime").unwrap_or_default();
    let mut uptime = String::new();
    if let Some(first) = uptime_raw.split_whitespace().next() {
        uptime = format_up(to_f64(first));
    }

    // Users: pm list users
    let mut users: Vec<Value> = Vec::new();
    let pu = run(&["pm", "list", "users"]);
    for line in pu.lines() {
        // e.g. "Users:" or "  0:Owner"
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

    // Software: pm list packages
    let mut software: Vec<Value> = Vec::new();
    let pkgs = run(&["pm", "list", "packages"]);
    for line in pkgs.lines() {
        let l = line.trim();
        if l.starts_with("package:") && software.len() < MAX_SOFTWARE {
            software.push(json!({"name": l.trim_start_matches("package:"), "version": "", "publisher": ""}));
        }
    }

    // Services are not supported on Android.
    let services: Vec<Value> = Vec::new();

    json!({
        "os": os, "cpu": cpu, "memory": memory, "disk": disk,
        "gpu": "", "board": board, "screen": "", "uptime": uptime,
        "users": users, "software": software, "services": services,
    })
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos", target_os = "android")))]
fn collect() -> Value {
    json!({
        "os": {"name":"","version":"","arch":""},
        "cpu": {"model":"","cores":0,"threads":0,"freq_mhz":0},
        "memory": {"total_gb":0.0,"available_gb":0.0},
        "disk": {"total_gb":0.0,"free_gb":0.0},
        "gpu": "", "board": {"vendor":"","model":""}, "screen": "", "uptime": "",
        "users": [], "software": [], "services": [],
    })
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

/// Collect the local hardware/software configuration and serialize it to the
/// fixed-shape JSON string transported back to the controller.
pub fn collect_config_info() -> String {
    match serde_json::to_string(&collect()) {
        Ok(s) => s,
        Err(_) => String::new(),
    }
}
