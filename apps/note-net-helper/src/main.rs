//! `note-net-helper` — the privileged half of the setup address (decision D10).
//!
//! `authorize` records consent. `run` is the lease server: it is the only
//! command that changes interfaces, and only by adding or removing
//! `192.168.4.1/32` on `lo0` and binding port 80 there. Killing the process
//! leaves the journal for the next start to reconcile; it does not remove an
//! alias a live `note-emu` may still be listening on.

use std::path::PathBuf;
use std::process::ExitCode;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use clap::{Parser, Subcommand};
use note_net_helper::{default_socket, serve, state_dir, write_auth, Helper, SystemPlatform};

#[derive(Parser)]
#[command(name = "note-net-helper", version, about = "Setup-address helper for NOTE Emulator")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve lease requests. Needs root for the alias and port 80.
    Run {
        #[arg(long)]
        socket: Option<PathBuf>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// UID allowed to request leases from a root-run helper.
        #[arg(long)]
        owner_uid: Option<u32>,
    },
    /// Record that the user allowed the setup address. One shot, at install.
    Authorize {
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Withdraw that permission. Running devices keep every other mode.
    Cancel {
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("note-net-helper: {err}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), String> {
    match Cli::parse().command {
        Command::Authorize { state_dir: dir } => {
            let path = dir.unwrap_or_else(state_dir).join("auth.json");
            write_auth(&path, "authorized")?;
            println!("authorized ({})", path.display());
        }
        Command::Cancel { state_dir: dir } => {
            let path = dir.unwrap_or_else(state_dir).join("auth.json");
            write_auth(&path, "cancelled")?;
            println!("cancelled ({})", path.display());
        }
        Command::Run { socket, state_dir: dir, owner_uid } => {
            let running_uid = unsafe { libc::getuid() };
            let owner_uid = match (running_uid, owner_uid) {
                (0, Some(uid)) if uid != 0 => uid,
                (0, _) => return Err("root helper requires --owner-uid for a non-root user".into()),
                (uid, Some(owner)) if uid != owner => return Err("--owner-uid must match the current user unless running as root".into()),
                (uid, _) => uid,
            };
            let dir = dir.unwrap_or_else(|| if running_uid == 0 { note_net_helper::system_state_dir(owner_uid) } else { state_dir() });
            if running_uid == 0 && dir != note_net_helper::system_state_dir(owner_uid) {
                return Err(format!("root helper state directory must be {}", note_net_helper::system_state_dir(owner_uid).display()));
            }
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            if running_uid == 0 {
                let meta = std::fs::symlink_metadata(&dir).map_err(|e| e.to_string())?;
                if !meta.is_dir() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
                    return Err(format!("root helper state directory must be root-owned and not group or world writable: {}", dir.display()));
                }
            }
            let socket = socket.unwrap_or_else(default_socket);
            if running_uid == 0 && socket != note_net_helper::system_socket(owner_uid) {
                return Err(format!("root helper socket must be {}", note_net_helper::system_socket(owner_uid).display()));
            }
            if let Some(parent) = socket.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let _ = std::fs::remove_file(&socket);
            let listener = std::os::unix::net::UnixListener::bind(&socket).map_err(|e| format!("bind {}: {e}", socket.display()))?;
            if running_uid == 0 {
                let path = std::ffi::CString::new(socket.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
                if unsafe { libc::chown(path.as_ptr(), owner_uid, u32::MAX) } != 0 {
                    return Err(format!("chown {}: {}", socket.display(), std::io::Error::last_os_error()));
                }
            }
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
            let platform = SystemPlatform::new(dir.join("auth.json"));
            let mut helper = Helper::open(platform, dir.join("leases.json")).map_err(|e| e.to_string())?;
            helper.startup_reconcile();
            eprintln!("note-net-helper: listening on {}", socket.display());
            // Runs until the process is killed. A kill is the restart case:
            // the journal stays, and the next `run` reconciles with whoever
            // still holds the listening socket.
            serve(listener, Arc::new(Mutex::new(helper)), Arc::new(AtomicBool::new(false)), owner_uid);
        }
    }
    Ok(())
}
