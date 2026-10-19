pub mod db;
pub mod migrations;

use std::path::PathBuf;

#[derive(Clone)]
pub struct DataPaths {
    pub root: PathBuf,
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub shots_dir: PathBuf,
    pub exports_dir: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn data_root_lock_excludes_second_instance_and_releases_on_close() {
        let root = tempfile::tempdir().unwrap();
        let paths = DataPaths {
            root: root.path().to_owned(),
            data_dir: root.path().join("data"),
            db_path: root.path().join("data/db.sqlite"),
            shots_dir: root.path().join("data/shots"),
            exports_dir: root.path().join("exports"),
        };
        let first = paths.lock_exclusive().unwrap();
        assert!(paths.lock_exclusive().is_err());
        drop(first);
        assert!(paths.lock_exclusive().is_ok());
    }
}

impl DataPaths {
    /// OS-held lock releases automatically after a crash; a PID file alone would not be sufficient.
    pub fn lock_exclusive(&self) -> Result<std::fs::File, String> {
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let lock = opts
            .open(self.root.join("instance.lock"))
            .map_err(|e| e.to_string())?;
        fs2::FileExt::try_lock_exclusive(&lock)
            .map_err(|_| "此数据目录已被另一个 TimeLens 内部版进程使用，或无法取得排他锁")?;
        Ok(lock)
    }
    pub fn new() -> Result<Self, std::io::Error> {
        let home = dirs::home_dir().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no home directory")
        })?;
        // Internal tasks must not include historical data from a personal upstream installation.
        let root = home.join(".timelens-internal");
        let data_dir = root.join("data");
        let db_path = data_dir.join("db.sqlite");
        let shots_dir = data_dir.join("shots");
        let exports_dir = root.join("exports");
        Ok(Self {
            root,
            data_dir,
            db_path,
            shots_dir,
            exports_dir,
        })
    }

    pub fn ensure_dirs(&self) -> Result<(), std::io::Error> {
        use std::fs;
        fs::create_dir_all(&self.data_dir)?;
        fs::create_dir_all(&self.shots_dir)?;
        fs::create_dir_all(&self.exports_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::Permissions::from_mode(0o700);
            let _ = fs::set_permissions(&self.root, mode.clone());
            let _ = fs::set_permissions(&self.data_dir, mode);
        }
        Ok(())
    }
}
