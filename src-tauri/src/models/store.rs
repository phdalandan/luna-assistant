use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::catalog::CatalogModel;

const PARTIAL_DIR: &str = "downloads";
const MANIFEST_EXTENSION: &str = "verified";

/// Model files live in their own directory, separate from settings and databases.
#[derive(Debug, Clone)]
pub struct ModelStore {
    dir: PathBuf,
}

impl ModelStore {
    pub fn open(dir: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(dir.join(PARTIAL_DIR))?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn model_path(&self, model: &CatalogModel) -> PathBuf {
        self.dir.join(&model.file_name)
    }

    pub fn partial_path(&self, model: &CatalogModel) -> PathBuf {
        self.dir
            .join(PARTIAL_DIR)
            .join(format!("{}.part", model.file_name))
    }

    fn manifest_path(&self, model: &CatalogModel) -> PathBuf {
        self.dir
            .join(format!("{}.{MANIFEST_EXTENSION}", model.file_name))
    }

    /// Bytes already downloaded for a paused or interrupted download.
    pub fn partial_len(&self, model: &CatalogModel) -> u64 {
        fs::metadata(self.partial_path(model)).map_or(0, |metadata| metadata.len())
    }

    /// Installed means verified against the catalogue checksum and still the expected size.
    pub fn is_installed(&self, model: &CatalogModel) -> bool {
        let verified = fs::read_to_string(self.manifest_path(model))
            .is_ok_and(|checksum| checksum.trim() == model.sha256);
        let complete =
            fs::metadata(self.model_path(model)).is_ok_and(|metadata| metadata.len() == model.size);
        verified && complete
    }

    /// Moves a verified download into place, then records it as installed.
    pub fn install(&self, model: &CatalogModel, verified_file: &Path) -> io::Result<()> {
        fs::rename(verified_file, self.model_path(model))?;
        fs::write(self.manifest_path(model), &model.sha256)
    }

    pub fn delete(&self, model: &CatalogModel) -> io::Result<()> {
        // The manifest goes first so a half-deleted model is never reported as installed.
        remove_if_exists(&self.manifest_path(model))?;
        remove_if_exists(&self.model_path(model))?;
        self.discard_partial(model)
    }

    pub fn discard_partial(&self, model: &CatalogModel) -> io::Result<()> {
        remove_if_exists(&self.partial_path(model))
    }
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::models::catalog;

    /// A temporary model store that is removed when dropped.
    pub struct TempStore {
        pub store: ModelStore,
    }

    impl TempStore {
        pub fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("luna-models-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            Self {
                store: ModelStore::open(dir).unwrap(),
            }
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.store.dir());
        }
    }

    pub fn small_model(content: &[u8]) -> CatalogModel {
        use sha2::{Digest, Sha256};
        CatalogModel {
            size: content.len() as u64,
            sha256: hex(&Sha256::digest(content)),
            ..catalog::find("qwen3-8b").unwrap().clone()
        }
    }

    pub fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn install_detection_requires_manifest_and_size() {
        let temp = TempStore::new("detect");
        let model = small_model(b"model");
        assert!(!temp.store.is_installed(&model));

        fs::write(temp.store.model_path(&model), b"model").unwrap();
        assert!(
            !temp.store.is_installed(&model),
            "unverified files are not installed"
        );

        let download = temp.store.partial_path(&model);
        fs::write(&download, b"model").unwrap();
        temp.store.install(&model, &download).unwrap();
        assert!(temp.store.is_installed(&model));

        fs::write(temp.store.model_path(&model), b"mode").unwrap();
        assert!(
            !temp.store.is_installed(&model),
            "truncated files are not installed"
        );
    }

    #[test]
    fn manifest_for_another_version_is_not_installed() {
        let temp = TempStore::new("version");
        let model = small_model(b"model");
        fs::write(temp.store.model_path(&model), b"model").unwrap();
        fs::write(temp.store.manifest_path(&model), "0".repeat(64)).unwrap();
        assert!(!temp.store.is_installed(&model));
    }

    #[test]
    fn delete_removes_model_manifest_and_partial_files() {
        let temp = TempStore::new("delete");
        let model = small_model(b"model");
        let download = temp.store.partial_path(&model);
        fs::write(&download, b"model").unwrap();
        temp.store.install(&model, &download).unwrap();
        fs::write(temp.store.partial_path(&model), b"mo").unwrap();

        temp.store.delete(&model).unwrap();
        assert!(!temp.store.is_installed(&model));
        assert!(!temp.store.model_path(&model).exists());
        assert_eq!(temp.store.partial_len(&model), 0);
        temp.store.delete(&model).unwrap();
    }
}
