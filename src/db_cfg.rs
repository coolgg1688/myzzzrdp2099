//! Portable sqlite-backed config override + audit log (zremote66).
//!
//! Layout (portable only): `<exe_dir>/app_data/zrdp.db`
//!
//! Tables:
//!   zrdb_cfg(uid PK, type VARCHAR(32), cfgs TEXT, status SMALLINT DEFAULT 1)
//!   zrdp_log(uid PK, type VARCHAR(32), newtime DATETIME)
//!
//! The default config row has `type='cfg0'`. Its `cfgs` is AES-256-CBC (PKCS7)
//! encrypted JSON. Key = hex(MD5("zhx@@13030882113")) (32 bytes), IV = the same
//! literal (16 bytes). If the DB is missing, unreadable, or decryption fails,
//! every accessor returns None and the caller keeps original behaviour.

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use md5::{Digest, Md5};
use rusqlite::{Connection, OptionalExtension};
use serde_derive::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DB_FILE_NAME: &str = "zrdp.db";
pub const APP_DATA_DIR: &str = "app_data";
pub const CFG0_TYPE: &str = "cfg0";

/// Key/IV seed literal provided by the operator.
const KEY_SRC: &[u8] = b"zhx@@13030882113";
const IV: &[u8] = b"zhx@@13030882113"; // exactly 16 bytes

type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

/// Runtime-overridable parameters carried by `cfg0`.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct CfgOverrides {
    pub id_server: Option<String>,
    pub relay_server: Option<String>,
    pub api_server: Option<String>,
    pub public_key: Option<String>,
    pub slogan: Option<String>,
    pub domain: Option<String>,
    pub app_name: Option<String>,
    pub permanent_password: Option<String>,
    /// 注册给谁
    pub reg_to: Option<String>,
    /// 注册日期 (YYYY-MM-DD)
    pub reg_date: Option<String>,
    /// 服务截止日期 (YYYY-MM-DD); 缺省时 = 编译日期 + 30 天
    pub sv_date: Option<String>,
}

fn aes_key() -> [u8; 32] {
    let digest = Md5::digest(KEY_SRC);
    let hex = format!("{:x}", digest);
    debug_assert_eq!(hex.len(), 32);
    let mut k = [0u8; 32];
    k.copy_from_slice(hex.as_bytes());
    k
}

fn aes_encrypt(plain: &[u8]) -> anyhow::Result<String> {
    let key = aes_key();
    let ct = Aes256CbcEnc::new(&key.into(), IV.into()).encrypt_padded_vec_mut::<Pkcs7>(plain);
    Ok(B64.encode(&ct))
}

fn aes_decrypt(b64: &str) -> anyhow::Result<Vec<u8>> {
    let data = B64.decode(b64)?;
    let key = aes_key();
    let pt = Aes256CbcDec::new(&key.into(), IV.into())
        .decrypt_padded_vec_mut::<Pkcs7>(&data)
        .map_err(|_| anyhow::anyhow!("aes decrypt/pad failed"))?;
    Ok(pt)
}

use std::fs;

/// DB path. Desktop: `<exe_dir>/app_data/zrdp.db` (sidecar, portable + installed
/// both read the same place). Android: app data dir (`Config::get_home()` +
/// `app_data/zrdp.db`) — there is no writable "exe" dir on Android.
///
/// zremote66 single-file mode: the self-extracting packer runs the payload from a
/// per-version extraction dir under `%LOCALAPPDATA%`, so `current_exe()` points
/// there and the user-placed sidecar `app_data/zrdp.db` would be missed. The
/// packer therefore exports `RUSTDESK_ORIG_EXE_DIR` (the real exe directory the
/// user launched) and `RUSTDESK_APPNAME`. When those are present we read/write the
/// sidecar at `<orig>/app_data/zrdp.db` instead, so the portable config is found
/// and authorizations persist beside the actual exe. The directory is created
/// on demand by `open_conn`.
fn db_path() -> PathBuf {
    #[cfg(target_os = "android")]
    {
        let mut d = crate::config::Config::get_home();
        if d.as_os_str().is_empty() {
            return PathBuf::new();
        }
        d.push(APP_DATA_DIR);
        d.join(DB_FILE_NAME)
    }
    #[cfg(not(target_os = "android"))]
    {
        // zremote66 single-file: prefer the real exe dir exported by the packer.
        if std::env::var("RUSTDESK_APPNAME").is_ok() {
            if let Ok(orig) = std::env::var("RUSTDESK_ORIG_EXE_DIR") {
                if !orig.trim().is_empty() {
                    return Path::new(&orig).join(APP_DATA_DIR).join(DB_FILE_NAME);
                }
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                return dir.join(APP_DATA_DIR).join(DB_FILE_NAME);
            }
        }
        PathBuf::new()
    }
}

fn open_conn() -> Option<Connection> {
    let path = db_path();
    if path.as_os_str().is_empty() {
        return None;
    }
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let conn = Connection::open(&path).ok()?;
    let _ = conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS zrdb_cfg (uid INTEGER PRIMARY KEY AUTOINCREMENT, type VARCHAR(32) NOT NULL, cfgs TEXT NOT NULL, status SMALLINT NOT NULL DEFAULT 1); \
         CREATE TABLE IF NOT EXISTS zrdp_log (uid INTEGER PRIMARY KEY AUTOINCREMENT, type VARCHAR(32) NOT NULL, newtime DATETIME NOT NULL);",
    );
    Some(conn)
}

/// Read + decrypt the latest enabled `cfg0`. Returns `None` on any failure so the
/// caller keeps original behaviour.
pub fn load_cfg0() -> Option<CfgOverrides> {
    let conn = open_conn()?;
    let row: Option<String> = conn
        .query_row(
            "SELECT cfgs FROM zrdb_cfg WHERE type=?1 AND status=1 ORDER BY uid DESC LIMIT 1",
            [CFG0_TYPE],
            |r| r.get(0),
        )
        .optional()
        .ok()?;
    let plain = aes_decrypt(&row?).ok()?;
    serde_json::from_slice::<CfgOverrides>(&plain).ok()
}

/// Append an audit event to `zrdp_log` with the current local time.
pub fn log_event(ty: &str) {
    if ty.len() > 32 {
        return;
    }
    if let Some(conn) = open_conn() {
        let _ = conn.execute(
            "INSERT INTO zrdp_log(type, newtime) VALUES(?1, datetime('now','localtime'))",
            rusqlite::params![ty],
        );
    }
}

extern "C" {
    // CRT/glibc atexit available on Windows (MSVC/MinGW), Linux, macOS and Android.
    fn atexit(callback: extern "C" fn());
}

extern "C" fn shutdown_cb() {
    // Best-effort audit write; ignore any failure (process is exiting).
    let path = db_path();
    if path.as_os_str().is_empty() {
        return;
    }
    if let Ok(conn) = Connection::open(&path) {
        let _ = conn.execute(
            "INSERT INTO zrdp_log(type, newtime) VALUES('shutdown', datetime('now','localtime'))",
            [],
        );
    }
}

/// Register a process-exit hook that records a `shutdown` audit event.
pub fn register_shutdown_hook() {
    unsafe {
        atexit(shutdown_cb);
    }
}

/// Serialize + AES-encrypt `ov`, then UPSERT into `zrdb_cfg` as `cfg0`:
/// update the latest row when present, insert when absent.
pub fn upsert_cfg0(ov: &CfgOverrides) -> bool {
    let Some(conn) = open_conn() else {
        return false;
    };
    let plain = match serde_json::to_vec(ov) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let enc = match aes_encrypt(&plain) {
        Ok(e) => e,
        Err(_) => return false,
    };
    let uid: Option<i64> = conn
        .query_row(
            "SELECT uid FROM zrdb_cfg WHERE type=?1 ORDER BY uid DESC LIMIT 1",
            [CFG0_TYPE],
            |r| r.get(0),
        )
        .unwrap_or(None);
    let res = match uid {
        Some(u) => conn.execute(
            "UPDATE zrdb_cfg SET cfgs=?1 WHERE uid=?2",
            rusqlite::params![enc, u],
        ),
        None => conn.execute(
            "INSERT INTO zrdb_cfg(type, cfgs, status) VALUES(?1, ?2, 1)",
            rusqlite::params![CFG0_TYPE, enc],
        ),
    };
    res.is_ok()
}

/// Decrypt an operator-issued auth code (AES-encrypted JSON), merge its fields
/// over the current cfg0, and UPSERT. Returns the merged overrides on success.
pub fn apply_auth_code(enc: &str) -> Result<CfgOverrides, String> {
    // Z远程协助: 授权码验证/覆盖规则——必须正确解密并解析为合法 JSON 才算有效授权，
    // 才能覆盖之前授权；无效输入不写入数据库(不覆盖之前正确授权)。
    // 错误提示仅"授权码错误"，不暴露 aes/加密/格式等技术细节。
    let plain = aes_decrypt(enc).map_err(|_| "授权码错误".to_owned())?;
    let auth: CfgOverrides =
        serde_json::from_slice(&plain).map_err(|_| "授权码错误".to_owned())?;
    let mut merged = load_cfg0().unwrap_or_default();
    macro_rules! merge {
        ($($f:ident),*) => {
            $( if auth.$f.is_some() { merged.$f = auth.$f; } )*
        };
    }
    merge!(
        id_server,
        relay_server,
        api_server,
        public_key,
        slogan,
        domain,
        app_name,
        permanent_password,
        reg_to,
        reg_date,
        sv_date
    );
    if !upsert_cfg0(&merged) {
        return Err("写入数据库失败".to_owned());
    }
    Ok(merged)
}

/// On first launch (no cfg0 row yet) seed a preset license: reg_to = the given
/// name, sv_date = build date + 365 days. Existing rows are left untouched so a
/// user-issued auth code is never overwritten.
pub fn ensure_preset(build_date: &str, reg_to: &str) {
    if load_cfg0().is_some() {
        return;
    }
    let bd = build_date.get(..10).unwrap_or(build_date);
    let sv = match chrono::NaiveDate::parse_from_str(bd, "%Y-%m-%d") {
        Ok(d) => d
            .checked_add_signed(chrono::Duration::days(365))
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| bd.to_string()),
        Err(_) => bd.to_string(),
    };
    let ov = CfgOverrides {
        reg_to: Some(reg_to.to_owned()),
        reg_date: Some(bd.to_string()),
        sv_date: Some(sv),
        ..Default::default()
    };
    let _ = upsert_cfg0(&ov);
}
