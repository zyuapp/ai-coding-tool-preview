//! Reading another browser's cookie database: Chromium's, whose values are encrypted under a key
//! its keyring holds, and Firefox's, whose are not.

use std::collections::HashMap;
use std::path::Path;

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

use super::cookie_on_site;

/// One cookie as another browser stored it, already decrypted. `expires` is in Unix seconds, None for a session cookie.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredCookie {
    pub host: String,
    pub name: String,
    pub value: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub expires: Option<i64>,
    pub same_site: SameSite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SameSite {
    Unspecified,
    NoRestriction,
    Lax,
    Strict,
}

impl SameSite {
    fn from_index(index: i64) -> Self {
        match index {
            0 => SameSite::NoRestriction,
            1 => SameSite::Lax,
            2 => SameSite::Strict,
            _ => SameSite::Unspecified,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CookieFamily {
    Chromium,
    Firefox,
}

/// Seconds between 1601, where Chromium counts from, and 1970.
const CHROMIUM_EPOCH_OFFSET: i64 = 11_644_473_600;
/// Chromium prefixes a value with the SHA-256 of its host from this cookie database version on.
const HASHED_VALUES_VERSION: i64 = 24;
const CBC_IV: [u8; 16] = [0x20; 16];

pub type ChromiumKey = [u8; 16];

/// A Chromium key from the password its keyring holds.
pub fn chromium_key(password: &str, iterations: u32) -> ChromiumKey {
    let mut key = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password.as_bytes(), b"saltysalt", iterations, &mut key);
    key
}

/// The browser keeps its database open and may be writing to it, so the file and its write-ahead
/// log are read from a private copy that is removed afterwards.
fn with_copy<T>(file: &Path, read: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> std::io::Result<T> {
    let folder = std::env::temp_dir().join(format!("aic-cookies-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&folder)?;
    let result = (|| {
        let copy = folder.join("cookies.sqlite");
        std::fs::copy(file, &copy)?;
        for suffix in ["-wal", "-journal"] {
            let mut source = file.as_os_str().to_owned();
            source.push(suffix);
            let mut target = copy.as_os_str().to_owned();
            target.push(suffix);
            let _ = std::fs::copy(&source, &target);
        }
        let database = Connection::open_with_flags(&copy, OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(std::io::Error::other)?;
        read(&database).map_err(std::io::Error::other)
    })();
    let _ = std::fs::remove_dir_all(&folder);
    result
}

fn has_column(database: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut statement = database.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = statement.query_map([], |row| row.get::<_, String>(1))?;
    for name in names {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn chromium_expiry() -> String {
    format!("CASE WHEN expires_utc = 0 THEN 0 ELSE expires_utc / 1000000 - {CHROMIUM_EPOCH_OFFSET} END")
}

fn chromium_filter(database: &Connection) -> rusqlite::Result<String> {
    // Cookies partitioned under another site's frame have nowhere to go in a session without partitions.
    let partitioned = if has_column(database, "cookies", "top_frame_site_key")? { "AND top_frame_site_key = ''" } else { "" };
    Ok(format!("WHERE (expires_utc = 0 OR {} > ?1) {partitioned}", chromium_expiry()))
}

/// Firefox stored expiry in seconds and later moved to milliseconds.
fn firefox_expiry(expiry: f64) -> f64 {
    if expiry > 100_000_000_000.0 { expiry / 1000.0 } else { expiry }
}

fn now_seconds() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|elapsed| elapsed.as_secs_f64()).unwrap_or(0.0)
}

/// How many unexpired cookies each host holds, which is all listing sites needs: nothing is decrypted.
pub fn read_cookie_hosts(family: CookieFamily, file: &Path) -> std::io::Result<Vec<(String, i64)>> {
    let now = now_seconds();
    with_copy(file, |database| match family {
        CookieFamily::Firefox => {
            let mut statement = database.prepare("SELECT host, expiry FROM moz_cookies WHERE originAttributes = ''")?;
            let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)))?;
            let mut counts: Vec<(String, i64)> = Vec::new();
            let mut index: HashMap<String, usize> = HashMap::new();
            for row in rows {
                let (host, expiry) = row?;
                if firefox_expiry(expiry) <= now {
                    continue;
                }
                match index.get(&host) {
                    Some(&at) => counts[at].1 += 1,
                    None => {
                        index.insert(host.clone(), counts.len());
                        counts.push((host, 1));
                    }
                }
            }
            Ok(counts)
        }
        CookieFamily::Chromium => {
            let sql = format!("SELECT host_key AS host, COUNT(*) AS cookies FROM cookies {} GROUP BY host_key", chromium_filter(database)?);
            let mut statement = database.prepare(&sql)?;
            let rows = statement.query_map([now as i64], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
            rows.collect()
        }
    })
}

fn has_control_characters(value: &str) -> bool {
    value.chars().any(|ch| ch <= '\u{1f}' || ch == '\u{7f}' || ch == '\u{fffd}')
}

/// Decrypts one Chromium value, or None when none of the keys open it.
pub fn decrypt_chromium_value(encrypted: &[u8], host: &str, keys: &[ChromiumKey], hashed: bool) -> Option<String> {
    let body = encrypted.get(3..)?;
    for key in keys {
        let Ok(plain) = cbc::Decryptor::<aes::Aes128>::new(key.into(), &CBC_IV.into()).decrypt_padded_vec_mut::<Pkcs7>(body) else {
            continue;
        };
        let plain = if hashed {
            if plain.len() < 32 || plain[..32] != Sha256::digest(host.as_bytes())[..] {
                continue;
            }
            plain[32..].to_vec()
        } else {
            plain
        };
        // A wrong key can still unpad cleanly; what it yields is not text a cookie could hold.
        let value = String::from_utf8_lossy(&plain);
        if has_control_characters(&value) {
            continue;
        }
        return Some(value.into_owned());
    }
    None
}

struct ChromiumRow {
    host: String,
    name: String,
    value: String,
    encrypted: Vec<u8>,
    path: String,
    expires: i64,
    secure: bool,
    http_only: bool,
    same_site: i64,
}

/// Every unexpired cookie for the given sites, and how many no key opened.
pub fn read_cookies(
    family: CookieFamily,
    file: &Path,
    sites: &[String],
    keys: &mut dyn FnMut(&str) -> Result<Vec<ChromiumKey>, String>,
) -> Result<(Vec<StoredCookie>, usize), ReadError> {
    let now = now_seconds();
    let on_sites = |host: &str| sites.iter().any(|site| cookie_on_site(host, site));
    if family == CookieFamily::Firefox {
        let rows = with_copy(file, |database| {
            let mut statement = database.prepare("SELECT host, name, value, path, expiry, isSecure, isHttpOnly, sameSite FROM moz_cookies WHERE originAttributes = ''")?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, f64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                ))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(ReadError::Io)?;
        let cookies = rows
            .into_iter()
            .filter(|row| on_sites(&row.0) && firefox_expiry(row.4) > now)
            .map(|(host, name, value, path, expiry, secure, http_only, same_site)| StoredCookie {
                host,
                name,
                value,
                path: path.filter(|path| !path.is_empty()).unwrap_or_else(|| "/".into()),
                secure: secure != 0,
                http_only: http_only != 0,
                expires: Some(firefox_expiry(expiry).floor() as i64),
                same_site: SameSite::from_index(same_site.unwrap_or(-1)),
            })
            .collect();
        return Ok((cookies, 0));
    }
    let (rows, version) = with_copy(file, |database| {
        let version: i64 = database
            .query_row("SELECT value FROM meta WHERE key = 'version'", [], |row| row.get::<_, rusqlite::types::Value>(0))
            .map(|value| match value {
                rusqlite::types::Value::Integer(number) => number,
                rusqlite::types::Value::Text(text) => text.parse().unwrap_or(0),
                _ => 0,
            })
            .unwrap_or(0);
        let sql = format!(
            "SELECT host_key AS host, name, value, encrypted_value AS encrypted, path, {} AS expires, is_secure AS secure, is_httponly AS httpOnly, samesite AS sameSite FROM cookies {}",
            chromium_expiry(),
            chromium_filter(database)?
        );
        let mut statement = database.prepare(&sql)?;
        let rows = statement.query_map([now as i64], |row| {
            Ok(ChromiumRow {
                host: row.get(0)?,
                name: row.get(1)?,
                value: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                encrypted: row.get::<_, Option<Vec<u8>>>(3)?.unwrap_or_default(),
                path: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                expires: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                secure: row.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
                http_only: row.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0,
                same_site: row.get::<_, Option<i64>>(8)?.unwrap_or(-1),
            })
        })?;
        Ok((rows.collect::<rusqlite::Result<Vec<_>>>()?, version))
    })
    .map_err(ReadError::Io)?;
    let mut keyring: HashMap<String, Vec<ChromiumKey>> = HashMap::new();
    let mut cookies = Vec::new();
    let mut skipped = 0usize;
    for row in rows.into_iter().filter(|row| on_sites(&row.host)) {
        let mut value = Some(row.value.clone());
        if !row.encrypted.is_empty() {
            let prefix = String::from_utf8_lossy(&row.encrypted[..row.encrypted.len().min(3)]).into_owned();
            if prefix != "v10" && prefix != "v11" {
                skipped += 1;
                continue;
            }
            if !keyring.contains_key(&prefix) {
                let found = keys(&prefix).map_err(ReadError::Keys)?;
                keyring.insert(prefix.clone(), found);
            }
            value = decrypt_chromium_value(&row.encrypted, &row.host, &keyring[&prefix], version >= HASHED_VALUES_VERSION);
        }
        let Some(value) = value else {
            skipped += 1;
            continue;
        };
        cookies.push(StoredCookie {
            host: row.host,
            name: row.name,
            value,
            path: if row.path.is_empty() { "/".into() } else { row.path },
            secure: row.secure,
            http_only: row.http_only,
            expires: (row.expires != 0).then_some(row.expires),
            same_site: SameSite::from_index(row.same_site),
        });
    }
    Ok((cookies, skipped))
}

#[derive(Debug)]
pub enum ReadError {
    Io(std::io::Error),
    Keys(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncryptMut;

    fn encrypt(plain: &[u8], key: &ChromiumKey) -> Vec<u8> {
        let mut out = b"v10".to_vec();
        out.extend(cbc::Encryptor::<aes::Aes128>::new(key.into(), &CBC_IV.into()).encrypt_padded_vec_mut::<Pkcs7>(plain));
        out
    }

    #[test]
    fn decrypts_with_the_right_key_and_checks_the_host_hash() {
        let key = chromium_key("peanuts", 1);
        let wrong = chromium_key("other", 1);
        let plain = encrypt(b"session=abc", &key);
        assert_eq!(decrypt_chromium_value(&plain, "a.com", &[wrong, key], false).as_deref(), Some("session=abc"));
        let mut hashed = Sha256::digest(b"a.com").to_vec();
        hashed.extend_from_slice(b"token");
        let hashed = encrypt(&hashed, &key);
        assert_eq!(decrypt_chromium_value(&hashed, "a.com", &[key], true).as_deref(), Some("token"));
        assert_eq!(decrypt_chromium_value(&hashed, "b.com", &[key], true), None);
    }

    #[test]
    fn reads_unexpired_chromium_cookies_for_the_chosen_sites() {
        let folder = std::env::temp_dir().join(format!("act-cookie-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("Cookies");
        let key = chromium_key("pw", 1003);
        {
            let database = Connection::open(&file).unwrap();
            database.execute_batch("CREATE TABLE meta(key TEXT, value TEXT); INSERT INTO meta VALUES('version', '20');
                CREATE TABLE cookies(host_key TEXT, name TEXT, value TEXT, encrypted_value BLOB, path TEXT, expires_utc INTEGER, is_secure INTEGER, is_httponly INTEGER, samesite INTEGER, top_frame_site_key TEXT);").unwrap();
            let future = (now_seconds() as i64 + 3600 + CHROMIUM_EPOCH_OFFSET) * 1_000_000;
            let past = (now_seconds() as i64 - 3600 + CHROMIUM_EPOCH_OFFSET) * 1_000_000;
            let insert = "INSERT INTO cookies VALUES(?1, ?2, '', ?3, '/', ?4, 1, 1, 1, ?5)";
            database.execute(insert, rusqlite::params![".a.com", "live", encrypt(b"v", &key), future, ""]).unwrap();
            database.execute(insert, rusqlite::params!["a.com", "old", encrypt(b"v", &key), past, ""]).unwrap();
            database.execute(insert, rusqlite::params!["b.com", "other", encrypt(b"v", &key), future, ""]).unwrap();
            database.execute(insert, rusqlite::params!["x.a.com", "framed", encrypt(b"v", &key), future, "https://c.com"]).unwrap();
        }
        let hosts = read_cookie_hosts(CookieFamily::Chromium, &file).unwrap();
        assert_eq!(hosts.iter().map(|(_, count)| count).sum::<i64>(), 2);
        let (cookies, skipped) = read_cookies(CookieFamily::Chromium, &file, &["a.com".into()], &mut |_| Ok(vec![key])).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].name, "live");
        assert!(cookies[0].http_only && cookies[0].secure);
        assert_eq!(cookies[0].same_site, SameSite::Lax);
        std::fs::remove_dir_all(folder).unwrap();
    }
}
