use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
};

use anyhow::{Context, Result};

use super::Credentials;

pub(super) fn read(path: &Path) -> Result<Credentials> {
    let metadata = fs::symlink_metadata(path).context("could not read credential file")?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "credential file must be a regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        anyhow::ensure!(
            metadata.permissions().mode().trailing_zeros() >= 6,
            "credential file permissions must be private (0600)"
        );
    }
    let mut data = Vec::new();
    let file = fs::File::open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let opened = file.metadata()?;
        anyhow::ensure!(
            opened.ino() == metadata.ino() && opened.dev() == metadata.dev(),
            "credential file changed while opening"
        );
    }
    file.take(128 * 1024 + 1).read_to_end(&mut data)?;
    anyhow::ensure!(
        data.len() <= 128 * 1024,
        "credential file exceeds size limit"
    );
    serde_json::from_slice(&data).map_err(|_error| anyhow::anyhow!("credential file is invalid"))
}

pub(super) fn write(path: &Path, credentials: &Credentials) -> Result<()> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut directory = fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory
        .create(parent)
        .context("could not create credential directory")?;
    let temporary = parent.join(format!(".credentials-{}.tmp", uuid::Uuid::new_v4()));
    let result = write_atomic(&temporary, path, credentials);
    if result.is_err() {
        let _removed = fs::remove_file(&temporary);
    }
    result
}

fn write_atomic(temporary: &Path, path: &Path, credentials: &Credentials) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(temporary)
        .context("could not create private credential file")?;
    file.write_all(&serde_json::to_vec(credentials)?)?;
    file.sync_all()?;
    fs::rename(temporary, path).context("could not save credentials atomically")?;
    Ok(())
}

pub(super) fn remove(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("could not remove credentials"),
    }
}
