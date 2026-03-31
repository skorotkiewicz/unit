// spawn.rs — True self-replication for unit
//
// A running unit can package itself (binary + state + prelude) into a
// single blob and spawn a new independent process — locally or on a
// remote machine. The nanobot metaphor becomes literal.
//
// Package format:
//   Magic: "UREP" (4 bytes)
//   Version: u8
//   Binary size: u64
//   State size: u64
//   Prelude size: u64
//   Binary bytes
//   State bytes
//   Prelude bytes

use std::time::Instant;

// ---------------------------------------------------------------------------
// Package format
// ---------------------------------------------------------------------------

const PACKAGE_MAGIC: &[u8; 4] = b"UREP";
const PACKAGE_VERSION: u8 = 1;
const HEADER_SIZE: usize = 4 + 1 + 8 + 8 + 8; // 29 bytes

/// Build a replication package: binary + state + prelude.
pub fn build_package(state: &[u8]) -> Result<Vec<u8>, String> {
    let binary = read_own_binary()?;
    let prelude = include_bytes!("prelude.fs");

    let total = HEADER_SIZE + binary.len() + state.len() + prelude.len();
    let mut buf = Vec::with_capacity(total);

    // Header.
    buf.extend_from_slice(PACKAGE_MAGIC);
    buf.push(PACKAGE_VERSION);
    buf.extend_from_slice(&(binary.len() as u64).to_be_bytes());
    buf.extend_from_slice(&(state.len() as u64).to_be_bytes());
    buf.extend_from_slice(&(prelude.len() as u64).to_be_bytes());

    // Payloads.
    buf.extend_from_slice(&binary);
    buf.extend_from_slice(state);
    buf.extend_from_slice(prelude);

    Ok(buf)
}

/// Estimate package size without building it.
pub fn package_size_estimate(state_size: usize) -> Result<usize, String> {
    let binary_size = read_own_binary()?.len();
    let prelude_size = include_bytes!("prelude.fs").len();
    Ok(HEADER_SIZE + binary_size + state_size + prelude_size)
}

/// Result of unpacking a replication package: (binary, state, prelude).
pub type UnpackedPackage = (Vec<u8>, Vec<u8>, Vec<u8>);

/// Unpack a replication package. Returns (binary, state, prelude).
pub fn unpack_package(data: &[u8]) -> Result<UnpackedPackage, String> {
    if data.len() < HEADER_SIZE {
        return Err("package too small".into());
    }
    if &data[0..4] != PACKAGE_MAGIC {
        return Err("bad magic".into());
    }
    if data[4] != PACKAGE_VERSION {
        return Err(format!("unsupported version {}", data[4]));
    }

    let binary_size = u64::from_be_bytes(data[5..13].try_into().unwrap()) as usize;
    let state_size = u64::from_be_bytes(data[13..21].try_into().unwrap()) as usize;
    let prelude_size = u64::from_be_bytes(data[21..29].try_into().unwrap()) as usize;

    let expected = HEADER_SIZE + binary_size + state_size + prelude_size;
    if data.len() < expected {
        return Err(format!("truncated: have {} need {}", data.len(), expected));
    }

    let mut pos = HEADER_SIZE;
    let binary = data[pos..pos + binary_size].to_vec();
    pos += binary_size;
    let state = data[pos..pos + state_size].to_vec();
    pos += state_size;
    let prelude = data[pos..pos + prelude_size].to_vec();

    Ok((binary, state, prelude))
}

/// Read this process's own executable binary.
fn read_own_binary() -> Result<Vec<u8>, String> {
    let path = std::env::current_exe().map_err(|e| format!("current_exe: {}", e))?;
    std::fs::read(&path).map_err(|e| format!("read binary: {}", e))
}

// ---------------------------------------------------------------------------
// Child tracking
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct ChildInfo {
    pub pid: u32,
    pub port: u16,
    pub node_id: [u8; 8],
    pub spawned_at: Instant,
}

#[derive(Clone, Debug)]
pub struct SpawnState {
    pub children: Vec<ChildInfo>,
    pub generation: u32,
    pub parent_id: Option<[u8; 8]>,
    pub max_children: usize,
    pub accept_replicate: bool,
    pub quarantine: bool,
    pub last_spawn: Option<Instant>,
    pub spawn_cooldown_secs: u64,
}

impl Default for SpawnState {
    fn default() -> Self {
        Self::new()
    }
}

impl SpawnState {
    pub fn new() -> Self {
        SpawnState {
            children: Vec::new(),
            generation: 0,
            parent_id: None,
            max_children: 10,
            accept_replicate: true,
            quarantine: false,
            last_spawn: None,
            spawn_cooldown_secs: 30,
        }
    }

    pub fn can_spawn(&self) -> Result<(), String> {
        if self.quarantine {
            return Err("quarantine active".into());
        }
        if self.children.len() >= self.max_children {
            return Err(format!("max children reached ({})", self.max_children));
        }
        if let Some(last) = self.last_spawn {
            let elapsed = last.elapsed().as_secs();
            if elapsed < self.spawn_cooldown_secs {
                return Err(format!(
                    "cooldown: {}s remaining",
                    self.spawn_cooldown_secs - elapsed
                ));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Local spawn
// ---------------------------------------------------------------------------

/// Spawn a new unit process on the local machine.
/// Returns (pid, port, child_id).
pub fn spawn_local(
    package: &[u8],
    parent_port: u16,
    child_generation: u32,
) -> Result<(u32, u16, [u8; 8]), String> {
    let (binary, state, _prelude) = unpack_package(package)?;

    // Generate a child ID.
    let mut child_id = [0u8; 8];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut child_id);
    }
    let child_hex: String = child_id.iter().map(|b| format!("{:02x}", b)).collect();

    // Create spawn directory.
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let spawn_dir = format!("{}/.unit/spawn/{}", home, child_hex);
    std::fs::create_dir_all(&spawn_dir).map_err(|e| format!("mkdir: {}", e))?;

    // Write binary.
    let bin_path = format!("{}/unit", spawn_dir);
    std::fs::write(&bin_path, &binary).map_err(|e| format!("write binary: {}", e))?;

    // chmod +x
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&bin_path, perms).map_err(|e| format!("chmod: {}", e))?;
    }

    // Write state.
    let state_dir = format!("{}/.unit/{}", home, child_hex);
    std::fs::create_dir_all(&state_dir).map_err(|e| format!("mkdir state: {}", e))?;
    std::fs::write(format!("{}/state.bin", state_dir), &state)
        .map_err(|e| format!("write state: {}", e))?;

    // Write the node-id file so the child boots with this identity.
    std::fs::write(
        format!("{}/.unit/spawn/{}/node-id", home, child_hex),
        &child_hex,
    )
    .map_err(|e| format!("write node-id: {}", e))?;

    // Pick a port: 0 = OS-assigned.
    let child_port = 0u16;

    // Launch the child process.
    let child = std::process::Command::new(&bin_path)
        .env("UNIT_PORT", child_port.to_string())
        .env("UNIT_PEERS", format!("127.0.0.1:{}", parent_port))
        .env("UNIT_GENERATION", child_generation.to_string())
        .env("UNIT_PARENT_ID", child_hex.clone())
        .env("UNIT_NODE_ID", child_hex.clone())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| format!("spawn: {}", e))?;

    let pid = child.id();

    Ok((pid, child_port, child_id))
}

// ---------------------------------------------------------------------------
// TCP replication (send package to remote host)
// ---------------------------------------------------------------------------

/// Send a replication package to a remote host via TCP.
pub fn send_package(addr: &str, package: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::net::TcpStream;
    use std::time::Duration;

    let mut stream = TcpStream::connect(addr).map_err(|e| format!("connect {}: {}", addr, e))?;
    stream.set_write_timeout(Some(Duration::from_secs(30))).ok();

    // Send length prefix + package.
    let len_bytes = (package.len() as u64).to_be_bytes();
    stream
        .write_all(&len_bytes)
        .map_err(|e| format!("write len: {}", e))?;
    stream
        .write_all(package)
        .map_err(|e| format!("write pkg: {}", e))?;

    Ok(())
}

/// Listen for incoming replication packages on a TCP port.
/// Runs in a background thread. Calls `on_receive` for each package.
pub fn start_replication_listener(port: u16) -> Result<std::sync::mpsc::Receiver<Vec<u8>>, String> {
    use std::io::Read;
    use std::net::TcpListener;

    let listener = TcpListener::bind(format!("127.0.0.1:{}", port))
        .map_err(|e| format!("bind TCP {}: {}", port, e))?;

    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(30)))
                .ok();

            // Read length prefix.
            let mut len_buf = [0u8; 8];
            if stream.read_exact(&mut len_buf).is_err() {
                continue;
            }
            let pkg_len = u64::from_be_bytes(len_buf) as usize;
            if pkg_len > 100_000_000 {
                // Sanity: reject > 100MB.
                continue;
            }

            // Read package.
            let mut pkg = vec![0u8; pkg_len];
            if stream.read_exact(&mut pkg).is_err() {
                continue;
            }

            // Validate header.
            if pkg.len() >= 4 && &pkg[0..4] == PACKAGE_MAGIC {
                let _ = tx.send(pkg);
            }
        }
    });

    Ok(rx)
}
