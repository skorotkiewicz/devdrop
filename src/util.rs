use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn current_os() -> &'static str {
    match env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    }
}

pub fn current_arch() -> &'static str {
    match env::consts::ARCH {
        "x86_64" | "x86" => "x64",
        "aarch64" => "arm64",
        _ => env::consts::ARCH,
    }
}

pub fn json_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn json_optional(value: Option<&str>) -> String {
    value.map(json_string).unwrap_or_else(|| "null".to_string())
}

pub fn fnv_bytes(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

pub fn hex_decode(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err("odd-length hex".into());
    }

    text.as_bytes()
        .chunks(2)
        .map(|chunk| Ok((hex_nibble(chunk[0])? << 4) | hex_nibble(chunk[1])?))
        .collect()
}

pub fn hex_decode_string(text: &str) -> Result<String, String> {
    String::from_utf8(hex_decode(text)?).map_err(|err| format!("bad utf-8 hex: {err}"))
}

fn hex_nibble(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(format!("invalid hex byte: {}", byte as char)),
    }
}

pub fn sql_optional(value: Option<&str>) -> String {
    value.map(sql_string).unwrap_or_else(|| "NULL".to_string())
}

pub fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

pub fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}
