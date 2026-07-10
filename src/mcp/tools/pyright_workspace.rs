// SPDX-License-Identifier: AGPL-3.0-or-later
use std::fs::{DirBuilder, Permissions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use tracing::{debug, info, warn};
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

pub(super) struct PrivateWorkspaceRoot {
    path: PathBuf,
}

impl PrivateWorkspaceRoot {
    pub(super) fn prepare(path: PathBuf) -> Result<Self> {
        ensure_private_directory(&path, true).with_context(|| {
            format!(
                "failed to prepare private Pyright workspace root: {}",
                path.display()
            )
        })?;

        let root = Self { path };
        root.clean_stale_workspaces()?;
        Ok(root)
    }

    pub(super) fn create_request_directory(&self) -> Result<PathBuf> {
        for _ in 0..8 {
            let path = self.path.join(Uuid::new_v4().to_string());
            match create_private_directory(&path, false) {
                Ok(()) => return Ok(path),
                Err(err)
                    if err
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|io_err| io_err.kind() == ErrorKind::AlreadyExists) =>
                {
                    continue;
                }
                Err(err) => return Err(err).context("failed to create private request workspace"),
            }
        }
        bail!("failed to allocate a unique Pyright request workspace");
    }

    fn clean_stale_workspaces(&self) -> Result<()> {
        let mut removed = 0usize;
        for entry in std::fs::read_dir(&self.path).with_context(|| {
            format!(
                "failed to inspect Pyright workspace root: {}",
                self.path.display()
            )
        })? {
            let entry = entry.context("failed to inspect Pyright workspace entry")?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                warn!("ignoring non-UTF-8 entry in Pyright workspace root");
                continue;
            };
            let Ok(workspace_id) = Uuid::parse_str(name) else {
                debug!(entry = name, "ignoring non-CCR Pyright workspace entry");
                continue;
            };
            if workspace_id.hyphenated().to_string() != name {
                debug!(entry = name, "ignoring non-canonical workspace entry");
                continue;
            }

            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)
                .context("failed to inspect stale Pyright workspace")?;
            if metadata.file_type().is_symlink() {
                std::fs::remove_file(&path)
                    .context("failed to unlink stale Pyright workspace symlink")?;
                removed += 1;
            } else if metadata.is_dir() {
                // remove_dir_all does not traverse symlink entries. Limiting cleanup to
                // UUID-named direct children further confines deletion to CCR-owned paths.
                std::fs::remove_dir_all(&path)
                    .context("failed to remove stale Pyright workspace directory")?;
                removed += 1;
            } else {
                warn!(
                    entry = name,
                    "ignoring non-directory Pyright workspace entry"
                );
            }
        }

        if removed > 0 {
            info!(removed, "removed stale Pyright request workspaces");
        }
        Ok(())
    }
}

fn ensure_private_directory(path: &Path, recursive: bool) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => validate_directory(path, &metadata)?,
        Err(err) if err.kind() == ErrorKind::NotFound => create_private_directory(path, recursive)?,
        Err(err) => return Err(err.into()),
    }

    let metadata = std::fs::symlink_metadata(path)?;
    validate_directory(path, &metadata)?;
    set_private_permissions(path)?;
    Ok(())
}

fn create_private_directory(path: &Path, recursive: bool) -> Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(recursive);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(path)?;

    let metadata = std::fs::symlink_metadata(path)?;
    validate_directory(path, &metadata)?;
    set_private_permissions(path)?;
    Ok(())
}

fn validate_directory(path: &Path, metadata: &std::fs::Metadata) -> Result<()> {
    if metadata.file_type().is_symlink() {
        bail!(
            "private workspace path must not be a symlink: {}",
            path.display()
        );
    }
    if !metadata.is_dir() {
        bail!(
            "private workspace path must be a directory: {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<()> {
    std::fs::set_permissions(path, Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(path: &Path) -> Result<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn prepare_enforces_permissions_and_cleans_only_owned_entries() {
        let base = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("sentinel"), "keep").unwrap();

        let root = base.path().join("scratch");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, Permissions::from_mode(0o755)).unwrap();

        let stale_name = Uuid::new_v4().to_string();
        let stale = root.join(&stale_name);
        std::fs::create_dir(&stale).unwrap();
        std::os::unix::fs::symlink(outside.path(), stale.join("unsafe-link")).unwrap();

        let link_name = Uuid::new_v4().to_string();
        std::os::unix::fs::symlink(outside.path(), root.join(&link_name)).unwrap();

        let unrelated = root.join("operator-owned");
        std::fs::create_dir(&unrelated).unwrap();

        let prepared = PrivateWorkspaceRoot::prepare(root.clone()).unwrap();
        assert_eq!(prepared.path, root);
        assert_eq!(
            std::fs::metadata(&prepared.path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(!prepared.path.join(stale_name).exists());
        assert!(std::fs::symlink_metadata(prepared.path.join(link_name)).is_err());
        assert!(unrelated.exists());
        assert_eq!(
            std::fs::read_to_string(outside.path().join("sentinel")).unwrap(),
            "keep"
        );
    }

    #[cfg(unix)]
    #[test]
    fn request_directories_are_private() {
        let base = tempfile::tempdir().unwrap();
        let root = PrivateWorkspaceRoot::prepare(base.path().join("scratch")).unwrap();
        let request = root.create_request_directory().unwrap();

        assert_eq!(
            std::fs::metadata(request).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[test]
    fn workspace_root_must_not_be_a_symlink() {
        let base = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = base.path().join("scratch");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();

        assert!(PrivateWorkspaceRoot::prepare(link).is_err());
    }
}
