//! Runtime identity of the local GeoTIFF used to decode a displayed raster.
use std::{
    fs::Metadata,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FileRevision {
    path: PathBuf,
    len: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
    #[cfg(windows)]
    created: u64,
}

impl FileRevision {
    pub(super) fn capture(path: &Path, metadata: &Metadata) -> std::io::Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        #[cfg(windows)]
        use std::os::windows::fs::MetadataExt;
        Ok(Self {
            path: std::path::absolute(path)?,
            len: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            identity: (metadata.dev(), metadata.ino(), metadata.ctime(), metadata.ctime_nsec()),
            #[cfg(windows)]
            created: metadata.creation_time(),
        })
    }
}

impl super::Raster {
    /// Metadata only, called on the grid reader's worker, never by per-frame UI checks.
    /// Missing provenance is refused rather than pairing an arbitrary file with displayed pixels.
    pub fn check_original_file(&self, path: &Path, metadata: &Metadata) -> std::result::Result<(), String> {
        let current = FileRevision::capture(path, metadata).map_err(|e| e.to_string())?;
        match &self.source_file {
            Some(original) if *original == current => Ok(()),
            Some(_) => Err("The image file changed since its pixels were loaded. Use Reload Data, then Reload Grid.".into()),
            None => Err("The displayed image has no verified original file revision. Use Reload Data, then Reload Grid.".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn annotation_grid_revision_detects_replacement_with_the_same_length_and_modification_time() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("image.tif");
        let bytes = super::super::geotiff::write(&super::super::tests::small(), 0).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let raster = super::super::geotiff::read_file(&path, "image").unwrap();
        let original = std::fs::File::open(&path).unwrap();
        let metadata = original.metadata().unwrap();
        raster.check_original_file(&path, &metadata).unwrap();
        let replacement = folder.path().join("replacement.tif");
        std::fs::write(&replacement, &bytes).unwrap();
        let file = std::fs::File::options().write(true).open(&replacement).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(metadata.modified().unwrap())).unwrap();
        drop(file);
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let current = std::fs::metadata(&path).unwrap();
        assert_eq!(metadata.len(), current.len());
        assert_eq!(metadata.modified().unwrap(), current.modified().unwrap());
        // Metadata captured before replacement still describes the old file, not the live path.
        raster.check_original_file(&path, &metadata).unwrap();
        assert!(raster.check_original_file(&path, &current).unwrap_err().contains("Reload Data"));
        assert!(raster.clone().check_original_file(&path, &current).is_err());
    }
}
