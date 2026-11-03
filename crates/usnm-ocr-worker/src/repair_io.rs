//! Shared durability barriers for repair manifests, retained files and indexes.

use std::path::Path;

use anyhow::Context;

pub(crate) fn sync_file(file: &std::fs::File, path: &Path) -> anyhow::Result<()> {
    #[cfg(test)]
    sync_test::before(path, path, false)?;
    file.sync_all()
        .with_context(|| format!("syncing repair file {}", path.display()))
}

/// Include ancestors that an earlier interrupted attempt may have created.
pub(crate) fn sync_directories(dir: &Path) -> anyhow::Result<()> {
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let absolute = std::path::absolute(dir)?;
    let canonical = std::fs::canonicalize(dir)?;
    let mut synced = std::collections::BTreeSet::new();
    for path in absolute.ancestors().chain(canonical.ancestors()) {
        if !synced.insert(path.to_owned()) {
            continue;
        }
        let file = std::fs::File::open(path)
            .with_context(|| format!("opening repair directory {}", path.display()))?;
        #[cfg(test)]
        sync_test::before(dir, path, true)?;
        file.sync_all()
            .with_context(|| format!("syncing repair directory {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod sync_test {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    type Hook = Arc<dyn Fn(&Path, bool) -> std::io::Result<()> + Send + Sync>;
    static HOOKS: Mutex<Vec<(PathBuf, Hook)>> = Mutex::new(Vec::new());

    pub(crate) struct Guard(PathBuf);

    impl Drop for Guard {
        fn drop(&mut self) {
            HOOKS.lock().unwrap().retain(|(root, _)| root != &self.0);
        }
    }

    pub(crate) fn install(
        root: &Path,
        hook: impl Fn(&Path, bool) -> std::io::Result<()> + Send + Sync + 'static,
    ) -> Guard {
        HOOKS
            .lock()
            .unwrap()
            .push((root.to_owned(), Arc::new(hook)));
        Guard(root.to_owned())
    }

    pub(super) fn before(scope: &Path, path: &Path, directory: bool) -> std::io::Result<()> {
        let hook = HOOKS
            .lock()
            .unwrap()
            .iter()
            .find(|(root, _)| scope.starts_with(root))
            .map(|(_, hook)| Arc::clone(hook));
        if let Some(hook) = hook {
            hook(path, directory)?;
        }
        Ok(())
    }
}
