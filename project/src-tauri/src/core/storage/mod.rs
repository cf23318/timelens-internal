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
            opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
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
        let root = match std::env::var_os("TIMELENS_PROFILE_DIR") { Some(p)=>{let p=PathBuf::from(p); if !p.is_absolute(){return Err(std::io::ErrorKind::InvalidInput.into())} p},None=>home.join(".timelens-internal")};
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
        #[cfg(unix)]
        {
            // The pilot token lives under root. Anchor every child creation to an
            // O_NOFOLLOW directory handle so an existing link cannot redirect it.
            let root = secure_root(&self.root)?;
            let data = secure_child(&root, "data")?;
            let _shots = secure_child(&data, "shots")?;
            let _exports = secure_child(&root, "exports")?;
        }
        #[cfg(not(unix))]
        {
            ensure_real_dir(&self.root)?;
            ensure_real_dir(&self.data_dir)?;
            ensure_real_dir(&self.shots_dir)?;
            ensure_real_dir(&self.exports_dir)?;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn secure_root(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;

    if let Err(error) = std::fs::create_dir(path) {
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error);
        }
    }
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    secure_dir_mode(&file)?;
    Ok(file)
}

#[cfg(unix)]
fn secure_child(parent: &std::fs::File, name: &str) -> std::io::Result<std::fs::File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};

    let name =
        CString::new(name).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error);
        }
    }
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    secure_dir_mode(&file)?;
    Ok(file)
}

#[cfg(unix)]
fn secure_dir_mode(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(file.as_raw_fd(), &mut stat) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if stat.st_uid != unsafe { libc::geteuid() } || stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    }
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o700) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_real_dir(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    }
    Ok(())
}

#[cfg(test)]
mod directory_tests {
    use super::DataPaths;

    fn paths(root: std::path::PathBuf) -> DataPaths {
        let data_dir = root.join("data");
        DataPaths {
            db_path: data_dir.join("db.sqlite"),
            shots_dir: data_dir.join("shots"),
            exports_dir: root.join("exports"),
            root,
            data_dir,
        }
    }

    #[cfg(unix)]
    #[test]
    fn secure_dirs_reject_root_and_child_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        let root = temp.path().join(".timelens");
        symlink(&target, &root).unwrap();
        assert!(paths(root.clone()).ensure_dirs().is_err());
        assert!(!target.join("data").exists());
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
        std::fs::remove_file(&root).unwrap();
        std::fs::create_dir(&root).unwrap();
        symlink(&target, root.join("data")).unwrap();
        assert!(paths(root.clone()).ensure_dirs().is_err());
        assert!(!target.join("shots").exists());
    }

    #[cfg(unix)]
    #[test]
    fn secure_dirs_create_private_tree() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join(".timelens");
        paths(root.clone()).ensure_dirs().unwrap();
        for dir in [
            root.clone(),
            root.join("data"),
            root.join("data/shots"),
            root.join("exports"),
        ] {
            assert_eq!(
                std::fs::metadata(dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }
}

/// Create exactly one managed child, never following a pre-existing symlink.
pub fn private_subdir(parent: &std::path::Path, name: &str) -> std::io::Result<PathBuf> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    #[cfg(unix)]
    {
        let p = secure_root(parent)?;
        let _ = secure_child(&p, name)?;
    }
    #[cfg(not(unix))]
    {
        ensure_real_dir(&parent.join(name))?;
    }
    Ok(parent.join(name))
}
