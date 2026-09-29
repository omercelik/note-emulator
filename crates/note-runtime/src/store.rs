//! AVD directory, exclusive run lock, and crash-consistent flash commits (Spec §10).
//!
//! `flash.img` is `sha256 || payload`. A commit writes `flash.img.writing` and
//! renames it over `flash.img`. On open, a writing file whose hash matches is
//! promoted; anything else is deleted and the previous image stands.
//!
//! macOS `flock` is per process, so a second handle in this process would not
//! fail. The `busy` file (pid + nonce) is the exclusion check, and a dead pid
//! is a stale lock rather than a running device.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use note_core::{Error, Result};
use sha2::{Digest, Sha256};

/// Network modes an AVD can start with; see `HostMode`.
pub const NETWORK_MODES: &[&str] = &["disabled", "user", "setup", "shared"];

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AvdConfig {
    pub id: String,
    pub name: String,
    pub profile: String,
    pub source: String,
    pub sha256: String,
    #[serde(default)]
    pub network: Option<String>,
    #[serde(default)]
    pub legacy_battery: Option<serde_json::Value>,
    /// Dotenv file (mode 0600) whose `WIFI_SSID` / `WIFI_PASSWORD` the virtual AP uses, so
    /// firmware with compiled-in home Wi-Fi joins it. Only the path is stored here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wifi_env: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AvdInfo {
    pub config: AvdConfig,
}

pub struct RunningLock {
    path: PathBuf,
    nonce: String,
    file: File,
}

impl Drop for RunningLock {
    fn drop(&mut self) {
        if let Ok(text) = fs::read_to_string(&self.path) {
            if text.lines().nth(1) == Some(self.nonce.as_str()) {
                let _ = fs::remove_file(&self.path);
            }
        }
        unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN); }
    }
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Store {
        Store { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn create(&self, profile: &str, name: &str, image: &[u8], source: &Path) -> Result<String> {
        let id = new_id();
        let dir = self.avd_dir(&id);
        fs::create_dir_all(dir.join("firmware")).map_err(|e| Error::io("create avd", e))?;
        write_image(&dir.join("firmware/base.img"), image)?;
        write_image(&dir.join("flash.img"), image)?;
        let config = AvdConfig {
            id: id.clone(), name: name.into(), profile: profile.into(),
            source: source.display().to_string(), sha256: hex(&sha256(image)),
            network: None, legacy_battery: None, wifi_env: None,
        };
        write_json(&dir.join("config.json"), &config)?;
        File::create(dir.join("lock")).map_err(|e| Error::io("create lock", e))?;
        Ok(id)
    }

    pub fn list(&self) -> Result<Vec<AvdInfo>> {
        let dir = self.root.join("avd");
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&dir).map_err(|e| Error::io("read avd dir", e))? {
            let entry = entry.map_err(|e| Error::io("read avd entry", e))?;
            let config_path = entry.path().join("config.json");
            if !config_path.exists() {
                continue;
            }
            // A folder this store cannot read (another tool's format, a half-written config) is
            // left alone and skipped; it must not hide every other AVD.
            let Ok(text) = fs::read_to_string(&config_path) else { continue };
            match serde_json::from_str::<AvdConfig>(&text) {
                Ok(config) => out.push(AvdInfo { config }),
                Err(err) => eprintln!("note: skipping {}: {err}", entry.path().display()),
            }
        }
        out.sort_by(|a, b| a.config.id.cmp(&b.config.id));
        Ok(out)
    }

    pub fn lock(&self, id: &str) -> Result<RunningLock> {
        let dir = self.require(id)?;
        let busy = dir.join("busy");
        if let Some(pid) = live_pid(&busy)? {
            return Err(Error::DeviceAlreadyRunning { id: id.into(), pid });
        }
        let _ = fs::remove_file(&busy);
        let file = OpenOptions::new().create(true).read(true).write(true).open(dir.join("lock")).map_err(|e| Error::io("open lock", e))?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            return Err(Error::DeviceAlreadyRunning { id: id.into(), pid: 0 });
        }
        let nonce = new_id();
        let mut created = OpenOptions::new().write(true).create_new(true).open(&busy).map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                Error::DeviceAlreadyRunning { id: id.into(), pid: 0 }
            } else {
                Error::io("create busy", e)
            }
        })?;
        writeln!(created, "{}\n{nonce}", std::process::id()).map_err(|e| Error::io("write busy", e))?;
        Ok(RunningLock { path: busy, nonce, file })
    }

    pub fn commit(&self, id: &str, bytes: &[u8]) -> Result<()> {
        let dir = self.require(id)?;
        write_image(&dir.join("flash.img"), bytes)
    }

    pub fn flash(&self, id: &str) -> Result<Vec<u8>> {
        let dir = self.require(id)?;
        recover(&dir)?;
        read_image(&dir.join("flash.img"))
    }

    pub fn baseline(&self, id: &str) -> Result<Vec<u8>> {
        read_image(&self.require(id)?.join("firmware/base.img"))
    }

    pub fn wipe(&self, id: &str) -> Result<()> {
        self.refuse_if_running(id)?;
        let _lock = self.lock(id)?;
        let base = self.baseline(id)?;
        self.commit(id, &base)?;
        // Erasing means the next start is a first boot: drop the quick-boot state too (it could
        // pair with the baseline flash). Snapshots saved by name are the user's and stay.
        let snapshots = self.require(id)?.join("snapshots");
        let _ = fs::remove_file(snapshots.join("quickboot.snap"));
        let _ = fs::remove_file(snapshots.join("quickboot.flash-sha256"));
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        self.refuse_if_running(id)?;
        let dir = self.require(id)?;
        fs::remove_dir_all(&dir).map_err(|e| Error::io(format!("delete {}", dir.display()), e))
    }

    pub fn config(&self, id: &str) -> Result<AvdConfig> {
        let text = fs::read_to_string(self.require(id)?.join("config.json")).map_err(|e| Error::io("read config", e))?;
        serde_json::from_str(&text).map_err(|e| Error::BadRequest(e.to_string()))
    }

    pub fn set_config(&self, config: &AvdConfig) -> Result<()> {
        write_json(&self.require(&config.id)?.join("config.json"), config)
    }

    /// The network mode the AVD starts with (`disabled`, `user`, `setup`, `shared`).
    /// `disabled` clears it. A running instance keeps its mode until it restarts.
    pub fn set_network(&self, id: &str, mode: &str) -> Result<AvdConfig> {
        if !NETWORK_MODES.contains(&mode) {
            return Err(Error::BadRequest(format!("network mode must be one of {}", NETWORK_MODES.join(", "))));
        }
        let mut config = self.config(id)?;
        config.network = (mode != "disabled").then(|| mode.to_string());
        self.set_config(&config)?;
        Ok(config)
    }

    /// `run/<instance>/instance.json`, which `ndb -s` reads. The path is removed
    /// when the process drops it.
    pub fn publish_run(&self, instance: &str, avd: &str, sock: &Path) -> Result<PathBuf> {
        let dir = self.root.join("run").join(instance);
        fs::create_dir_all(&dir).map_err(|e| Error::io("create run dir", e))?;
        let path = dir.join("instance.json");
        let body = serde_json::json!({
            "instance": instance,
            "avd": avd,
            "pid": std::process::id(),
            "sock": sock.display().to_string(),
        });
        fs::write(&path, body.to_string()).map_err(|e| Error::io("write instance", e))?;
        Ok(path)
    }

    fn refuse_if_running(&self, id: &str) -> Result<()> {
        if let Some(pid) = live_pid(&self.require(id)?.join("busy"))? {
            return Err(Error::DeviceAlreadyRunning { id: id.into(), pid });
        }
        Ok(())
    }

    /// Named machine snapshots of an AVD (G7).
    pub fn snapshot_dir(&self, id: &str) -> Result<PathBuf> {
        Ok(self.require(id)?.join("snapshots"))
    }

    fn require(&self, id: &str) -> Result<PathBuf> {
        let dir = self.avd_dir(id);
        if !dir.join("config.json").exists() {
            return Err(Error::AvdNotFound(id.into()));
        }
        Ok(dir)
    }

    fn avd_dir(&self, id: &str) -> PathBuf {
        self.root.join("avd").join(format!("{id}.avd"))
    }
}

fn write_image(path: &Path, payload: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io("create image dir", e))?;
    }
    let tmp = path.with_extension("img.writing");
    let mut body = sha256(payload).to_vec();
    body.extend_from_slice(payload);
    fs::write(&tmp, &body).map_err(|e| Error::io("write image", e))?;
    fs::rename(&tmp, path).map_err(|e| Error::io("commit image", e))
}

fn read_image(path: &Path) -> Result<Vec<u8>> {
    let bytes = fs::read(path).map_err(|e| Error::io(format!("read {}", path.display()), e))?;
    if bytes.len() < 32 {
        return Err(Error::BadRequest(format!("{} is not a committed image", path.display())));
    }
    let (sum, payload) = bytes.split_at(32);
    if sha256(payload).as_slice() != sum {
        return Err(Error::BadRequest(format!("{} checksum does not match", path.display())));
    }
    Ok(payload.to_vec())
}

fn recover(dir: &Path) -> Result<()> {
    let writing = dir.join("flash.img.writing");
    // write_image uses `with_extension`, so the temp name is `flash.img.writing`
    // only when the path extension is replaced. `flash.img` + with_extension("img.writing")
    // is `flash.img.writing`. Match both that name and a truncated file beside it.
    let writing = if writing.exists() { writing } else { dir.join("flash.img.writing") };
    if !writing.exists() {
        return Ok(());
    }
    let bytes = fs::read(&writing).unwrap_or_default();
    let good = bytes.len() >= 32 && sha256(&bytes[32..]).as_slice() == &bytes[..32];
    if good {
        fs::rename(&writing, dir.join("flash.img")).map_err(|e| Error::io("promote image", e))?;
    } else {
        let _ = fs::remove_file(&writing);
    }
    Ok(())
}

fn live_pid(path: &Path) -> Result<Option<u32>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).map_err(|e| Error::io("read busy", e))?;
    let pid = text.lines().next().unwrap_or("0").parse::<u32>().unwrap_or(0);
    if pid_alive(pid) { Ok(Some(pid)) } else { Ok(None) }
}

fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let rc = unsafe { libc::kill(pid as i32, 0) };
    rc == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let text = serde_json::to_string_pretty(value).map_err(|e| Error::BadRequest(e.to_string()))?;
    let tmp = path.with_extension("json.writing");
    fs::write(&tmp, text).map_err(|e| Error::io("write json", e))?;
    fs::rename(&tmp, path).map_err(|e| Error::io("commit json", e))
}

fn new_id() -> String {
    let mut b = [0u8; 16];
    if File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b)).is_err() {
        b = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

#[cfg(test)]
mod list_tests {
    use super::*;

    #[test]
    fn a_foreign_or_broken_avd_folder_does_not_hide_the_others() {
        let root = std::env::temp_dir().join(format!("store-list-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = Store::new(&root);
        let id = store.create("note4", "Good", &vec![0xff; 4096], Path::new("fw.bin")).unwrap();
        let foreign = root.join("avd/09ce86c244.avd");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("config.json"), r#"{"backend":"qemu","profileID":"note4c","id":"09ce86c244"}"#).unwrap();
        fs::create_dir_all(root.join("avd/empty.avd")).unwrap();
        let listed = store.list().unwrap();
        assert_eq!(listed.iter().map(|a| a.config.id.clone()).collect::<Vec<_>>(), [id]);
        let _ = fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("note-store-{}-{}", std::process::id(), new_id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn network_mode_is_saved_in_the_config_and_disabled_clears_it() {
        let root = scratch();
        let store = Store::new(&root);
        let id = store.create("note4c", "net", b"image", &root.join("fw.bin")).unwrap();
        assert_eq!(store.config(&id).unwrap().network, None);
        store.set_network(&id, "setup").unwrap();
        assert_eq!(store.config(&id).unwrap().network.as_deref(), Some("setup"));
        assert!(store.set_network(&id, "bridged").is_err());
        assert_eq!(store.config(&id).unwrap().network.as_deref(), Some("setup"));
        store.set_network(&id, "disabled").unwrap();
        assert_eq!(store.config(&id).unwrap().network, None);
    }

    #[test]
    fn guest_bytes_survive_stop_wipe_restores_baseline_and_the_source_is_untouched() {
        let root = scratch();
        let store = Store::new(&root);
        let source = root.join("firmware.bin");
        fs::write(&source, b"baseline-image").unwrap();
        let source_bytes = fs::read(&source).unwrap();
        let id = store.create("note4", "desk", b"baseline-image", &source).unwrap();
        store.commit(&id, b"guest-wrote-this").unwrap();
        drop(store);
        let store = Store::new(&root);
        assert_eq!(store.flash(&id).unwrap(), b"guest-wrote-this");
        assert_eq!(store.baseline(&id).unwrap(), b"baseline-image");
        assert_eq!(fs::read(&source).unwrap(), source_bytes);
        let snaps = root.join("avd").join(format!("{id}.avd")).join("snapshots");
        fs::create_dir_all(&snaps).unwrap();
        for name in ["quickboot.snap", "quickboot.flash-sha256", "mine.snap"] { fs::write(snaps.join(name), b"x").unwrap(); }
        store.wipe(&id).unwrap();
        assert_eq!(store.flash(&id).unwrap(), b"baseline-image");
        assert!(!snaps.join("quickboot.snap").exists() && !snaps.join("quickboot.flash-sha256").exists());
        assert!(snaps.join("mine.snap").exists(), "named snapshots stay");
        assert_eq!(fs::read(&source).unwrap(), source_bytes);
    }

    #[test]
    fn a_held_lock_rejects_wipe_and_delete_and_a_partial_commit_is_discarded() {
        let root = scratch();
        let store = Store::new(&root);
        let id = store.create("note4", "desk", b"baseline-image", Path::new("src.bin")).unwrap();
        let held = store.lock(&id).expect("lock");
        let err = store.lock(&id).err().expect("second lock should fail");
        assert!(matches!(err, Error::DeviceAlreadyRunning { .. }));
        let err = store.wipe(&id).err().expect("wipe should fail");
        assert!(matches!(err, Error::DeviceAlreadyRunning { .. }));
        let err = store.delete(&id).err().expect("delete should fail");
        assert!(matches!(err, Error::DeviceAlreadyRunning { .. }));
        drop(held);

        let dir = root.join("avd").join(format!("{id}.avd"));
        fs::write(dir.join("flash.img.writing"), b"not-a-valid-commit").unwrap();
        assert_eq!(store.flash(&id).unwrap(), b"baseline-image");
        assert!(!dir.join("flash.img.writing").exists());

        let mut good = sha256(b"promoted").to_vec();
        good.extend_from_slice(b"promoted");
        fs::write(dir.join("flash.img.writing"), &good).unwrap();
        assert_eq!(store.flash(&id).unwrap(), b"promoted");

        fs::write(dir.join("busy"), "2147483000\nstale\n").unwrap();
        let _again = store.lock(&id).unwrap();
    }
}
