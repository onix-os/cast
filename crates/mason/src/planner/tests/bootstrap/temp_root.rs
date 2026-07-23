struct BootstrapTempRoot {
    path: PathBuf,
    cleanup: Option<crate::paths::BoundedTempTree>,
}

impl BootstrapTempRoot {
    fn new(root: tempfile::TempDir) -> Self {
        // Disarm TempDir before the first fallible cleanup operation. Once a
        // tree is retained, no error path may fall back to pathname-recursive
        // TempDir::drop removal.
        let path = root.keep();
        let cleanup = crate::paths::BoundedTempTree::retain(&path)
            .unwrap_or_else(|error| panic!("retain bounded bootstrap temporary root {path:?}: {error}"));
        Self {
            path,
            cleanup: Some(cleanup),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn create_private_directory(&self, relative: &Path, display: &Path) {
        self.cleanup
            .as_ref()
            .expect("bootstrap temporary root was already closed")
            .create_private_directory(relative, display)
            .unwrap_or_else(|error| {
                panic!("create and authenticate private bootstrap directory {display:?}: {error}")
            });
    }

    fn close(mut self) -> io::Result<()> {
        self.cleanup
            .take()
            .expect("bootstrap temporary root was already closed")
            .remove()
    }
}

impl Drop for BootstrapTempRoot {
    fn drop(&mut self) {
        let Some(cleanup) = self.cleanup.take() else {
            return;
        };
        if let Err(error) = cleanup.remove() {
            eprintln!("failed to remove bounded bootstrap temporary root {:?}: {error}", self.path);
            if !std::thread::panicking() {
                panic!("failed to remove bounded bootstrap temporary root {:?}: {error}", self.path);
            }
        }
    }
}
