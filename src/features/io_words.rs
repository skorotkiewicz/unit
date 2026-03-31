// io_words.rs — Host I/O operations for unit
//
// Pure functions for file, HTTP, shell, and environment access.
// Sandbox policy is enforced by the caller (VM), not here.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// File system
// ---------------------------------------------------------------------------

pub fn file_read(path: &str) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|e| format!("{}: {}", path, e))
}

pub fn file_write(path: &str, data: &[u8]) -> Result<(), String> {
    fs::write(path, data).map_err(|e| format!("{}: {}", path, e))
}

pub fn file_exists(path: &str) -> bool {
    fs::metadata(path).is_ok()
}

pub fn file_list(path: &str) -> Result<Vec<String>, String> {
    let entries = fs::read_dir(path).map_err(|e| format!("{}: {}", path, e))?;
    let mut names = Vec::new();
    for entry in entries.flatten() {
        names.push(entry.file_name().to_string_lossy().to_string());
    }
    names.sort();
    Ok(names)
}

pub fn file_delete(path: &str) -> Result<(), String> {
    fs::remove_file(path).map_err(|e| format!("{}: {}", path, e))
}

// ---------------------------------------------------------------------------
// HTTP (raw TCP, HTTP/1.1, no TLS)
// ---------------------------------------------------------------------------

fn parse_url(url: &str) -> Result<(String, u16, String), String> {
    let url = url.strip_prefix("http://").unwrap_or(url);
    let (host_port, path) = match url.find('/') {
        Some(i) => (&url[..i], url[i..].to_string()),
        None => (url, "/".to_string()),
    };
    let (host, port) = match host_port.rfind(':') {
        Some(i) => (
            &host_port[..i],
            host_port[i + 1..]
                .parse::<u16>()
                .map_err(|_| "bad port".to_string())?,
        ),
        None => (host_port, 80u16),
    };
    Ok((host.to_string(), port, path))
}

fn parse_http_response(data: &[u8]) -> Result<(Vec<u8>, u16), String> {
    // Find end of headers.
    let mut header_end = 0;
    for i in 0..data.len().saturating_sub(3) {
        if &data[i..i + 4] == b"\r\n\r\n" {
            header_end = i;
            break;
        }
    }
    if header_end == 0 && data.len() > 4 {
        return Err("no header boundary".to_string());
    }

    // Parse status from first line: "HTTP/1.x NNN ..."
    let header = String::from_utf8_lossy(&data[..header_end]);
    let status_str = header
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or("no status line")?;
    let status = status_str
        .parse::<u16>()
        .map_err(|_| format!("bad status: {}", status_str))?;

    let body = data[header_end + 4..].to_vec();
    Ok((body, status))
}

pub fn http_get(url: &str) -> Result<(Vec<u8>, u16), String> {
    let (host, port, path) = parse_url(url)?;
    let addr = format!("{}:{}", host, port);

    let mut stream =
        TcpStream::connect(addr.as_str()).map_err(|e| format!("connect {}: {}", addr, e))?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(10))).ok();

    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: unit/0.4.0\r\n\r\n",
        path, host
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("write: {}", e))?;

    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .map_err(|e| format!("read: {}", e))?;
    parse_http_response(&buf)
}

pub fn http_post(url: &str, body: &[u8]) -> Result<(Vec<u8>, u16), String> {
    let (host, port, path) = parse_url(url)?;
    let addr = format!("{}:{}", host, port);

    let mut stream =
        TcpStream::connect(addr.as_str()).map_err(|e| format!("connect {}: {}", addr, e))?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(10))).ok();

    let req = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\nUser-Agent: unit/0.4.0\r\n\r\n",
        path, host, body.len()
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("write: {}", e))?;
    stream
        .write_all(body)
        .map_err(|e| format!("write body: {}", e))?;

    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .map_err(|e| format!("read: {}", e))?;
    parse_http_response(&buf)
}

// ---------------------------------------------------------------------------
// System
// ---------------------------------------------------------------------------

pub fn shell_exec(cmd: &str) -> Result<(Vec<u8>, i32), String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .map_err(|e| format!("exec: {}", e))?;
    let exit_code = output.status.code().unwrap_or(-1);
    Ok((output.stdout, exit_code))
}

pub fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

pub fn timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
