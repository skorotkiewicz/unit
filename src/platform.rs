// platform.rs — Platform abstraction traits for unit
//
// Defines the interface between the Forth VM and its host environment.
// Two implementations: native (std-based) and WASM (browser stubs).
// Future targets (embedded, WASI) implement the same traits.
//
// The core VM (stack, dictionary, interpreter) is platform-independent.
// Only I/O, networking, timing, and persistence vary by platform.

/// Platform capabilities — what the host environment provides.
pub trait Platform {
    /// Read a file's contents. Returns bytes or error.
    fn file_read(&self, path: &str) -> Result<Vec<u8>, String>;
    /// Write bytes to a file.
    fn file_write(&self, path: &str, data: &[u8]) -> Result<(), String>;
    /// Check if a file/path exists.
    fn file_exists(&self, path: &str) -> bool;
    /// List directory contents.
    fn file_list(&self, path: &str) -> Result<Vec<String>, String>;
    /// Delete a file.
    fn file_delete(&self, path: &str) -> Result<(), String>;

    /// HTTP GET. Returns (body, status_code) or error.
    fn http_get(&self, url: &str) -> Result<(Vec<u8>, u16), String>;
    /// HTTP POST. Returns (response_body, status_code) or error.
    fn http_post(&self, url: &str, body: &[u8]) -> Result<(Vec<u8>, u16), String>;

    /// Execute a shell command. Returns (stdout, exit_code) or error.
    fn shell_exec(&self, cmd: &str) -> Result<(Vec<u8>, i32), String>;
    /// Read an environment variable.
    fn env_var(&self, name: &str) -> Option<String>;

    /// Current unix timestamp in seconds.
    fn timestamp(&self) -> i64;
    /// Sleep for milliseconds.
    fn sleep_ms(&self, ms: u64);

    /// Read random bytes for seeding.
    fn random_bytes(&self, buf: &mut [u8]);

    /// Save persistent state.
    fn save_state(&self, key: &str, data: &[u8]) -> Result<(), String>;
    /// Load persistent state.
    fn load_state(&self, key: &str) -> Option<Vec<u8>>;

    /// Platform name for display.
    fn name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// Native platform (default, std-based)
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub struct NativePlatform;

#[cfg(not(target_arch = "wasm32"))]
impl NativePlatform {
    pub fn new() -> Self {
        NativePlatform
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Platform for NativePlatform {
    fn file_read(&self, path: &str) -> Result<Vec<u8>, String> {
        crate::features::io_words::file_read(path)
    }
    fn file_write(&self, path: &str, data: &[u8]) -> Result<(), String> {
        crate::features::io_words::file_write(path, data)
    }
    fn file_exists(&self, path: &str) -> bool {
        crate::features::io_words::file_exists(path)
    }
    fn file_list(&self, path: &str) -> Result<Vec<String>, String> {
        crate::features::io_words::file_list(path)
    }
    fn file_delete(&self, path: &str) -> Result<(), String> {
        crate::features::io_words::file_delete(path)
    }
    fn http_get(&self, url: &str) -> Result<(Vec<u8>, u16), String> {
        crate::features::io_words::http_get(url)
    }
    fn http_post(&self, url: &str, body: &[u8]) -> Result<(Vec<u8>, u16), String> {
        crate::features::io_words::http_post(url, body)
    }
    fn shell_exec(&self, cmd: &str) -> Result<(Vec<u8>, i32), String> {
        crate::features::io_words::shell_exec(cmd)
    }
    fn env_var(&self, name: &str) -> Option<String> {
        crate::features::io_words::env_var(name)
    }
    fn timestamp(&self) -> i64 {
        crate::features::io_words::timestamp()
    }
    fn sleep_ms(&self, ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    fn random_bytes(&self, buf: &mut [u8]) {
        // Read from /dev/urandom (same as mesh.rs node ID generation).
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            use std::io::Read;
            let _ = f.read_exact(buf);
        }
    }
    fn save_state(&self, key: &str, data: &[u8]) -> Result<(), String> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let dir = format!("{}/.unit", home);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(format!("{}/{}", dir, key), data).map_err(|e| e.to_string())
    }
    fn load_state(&self, key: &str) -> Option<Vec<u8>> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        std::fs::read(format!("{}/.unit/{}", home, key)).ok()
    }
    fn name(&self) -> &str {
        "native"
    }
}

// ---------------------------------------------------------------------------
// WASM platform (browser stubs)
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
pub struct WasmPlatform;

#[cfg(target_arch = "wasm32")]
impl WasmPlatform {
    pub fn new() -> Self {
        WasmPlatform
    }
}

#[cfg(target_arch = "wasm32")]
impl Platform for WasmPlatform {
    fn file_read(&self, _: &str) -> Result<Vec<u8>, String> {
        Err("no filesystem on WASM".into())
    }
    fn file_write(&self, _: &str, _: &[u8]) -> Result<(), String> {
        Err("no filesystem on WASM".into())
    }
    fn file_exists(&self, _: &str) -> bool {
        false
    }
    fn file_list(&self, _: &str) -> Result<Vec<String>, String> {
        Err("no filesystem on WASM".into())
    }
    fn file_delete(&self, _: &str) -> Result<(), String> {
        Err("no filesystem on WASM".into())
    }
    fn http_get(&self, _: &str) -> Result<(Vec<u8>, u16), String> {
        Err("use fetch() from JS".into())
    }
    fn http_post(&self, _: &str, _: &[u8]) -> Result<(Vec<u8>, u16), String> {
        Err("use fetch() from JS".into())
    }
    fn shell_exec(&self, _: &str) -> Result<(Vec<u8>, i32), String> {
        Err("no shell on WASM".into())
    }
    fn env_var(&self, _: &str) -> Option<String> {
        None
    }
    fn timestamp(&self) -> i64 {
        0
    } // JS should provide via extern
    fn sleep_ms(&self, _: u64) {} // no blocking sleep on WASM
    fn random_bytes(&self, buf: &mut [u8]) {
        // Simple fallback; JS should provide crypto.getRandomValues
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(137).wrapping_add(42);
        }
    }
    fn save_state(&self, _: &str, _: &[u8]) -> Result<(), String> {
        Err("use localStorage from JS".into())
    }
    fn load_state(&self, _: &str) -> Option<Vec<u8>> {
        None
    }
    fn name(&self) -> &str {
        "wasm"
    }
}
