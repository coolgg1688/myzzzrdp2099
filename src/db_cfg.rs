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
use std::path::PathBuf;

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

/// `<exe_dir>/app_data/zrdp.db` for the portable build.
fn db_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join(APP_DATA_DIR).join(DB_FILE_NAME);
        }
    }
    PathBuf::new()
}

fn open_conn() -> Option<Connection> {
    let path = db_path();
    if path.as_os_str().is_empty() {
        return None;
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
