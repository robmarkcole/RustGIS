//! Metadata-only observations of a linked file and the companions its reader opens: the archive
//! reload's landing check and the desktop's changed-on-disk monitor share them. Nothing here reads
//! a dataset's contents, so a rewrite that keeps size, times and identity is not seen.
use std::path::Path;

/// One file's size, modification time and identity (on unix the device, inode and status-change
/// time; on Windows the creation time), so replacing the object at a path is seen even when the
/// replacement keeps size and modification time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRevision {
    len: u64,
    modified: web_time::SystemTime,
    #[cfg(unix)]
    identity_change: (u64, u64, i64, i64),
    #[cfg(windows)]
    created: u64,
}

/// How a missing or empty file at a path counts.
#[derive(Clone, Copy, PartialEq)]
enum Presence {
    /// Missing is an error (an archive being validated).
    Required,
    /// Missing is `None`; an empty file is a revision (a monitored source, a Shapefile component).
    Optional,
    /// A SQLite `-wal`/`-journal`: missing or empty is `None`, since SQLite creates an empty
    /// `-wal` even for a read-only open of a clean WAL-mode file and cannot remove it again.
    Sidecar,
}

impl FileRevision {
    fn read(path: &Path, presence: Presence, noun: &str) -> Result<Option<Self>, String> {
        let shown = path.display();
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(e) if presence != Presence::Required && e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(format!("{shown}: could not observe {noun} metadata ({e}); finish writing or restore the file and retry"));
            }
        };
        if presence == Presence::Sidecar && metadata.len() == 0 {
            return Ok(None);
        }
        if !metadata.is_file() {
            let article = if noun == "archive" { "an" } else { "a" };
            return Err(format!("{shown}: expected {article} {noun} file; restore the file and retry"));
        }
        let modified = metadata
            .modified()
            .map_err(|e| format!("{shown}: could not observe the {noun} modification time ({e}); check file access and retry"))?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        #[cfg(windows)]
        use std::os::windows::fs::MetadataExt;
        Ok(Some(Self {
            len: metadata.len(),
            modified,
            #[cfg(unix)]
            identity_change: (metadata.dev(), metadata.ino(), metadata.ctime(), metadata.ctime_nsec()),
            #[cfg(windows)]
            created: metadata.creation_time(),
        }))
    }
}

/// A linked file and its companions, observed together. `-shm` is SQLite reader coordination,
/// not content, and is deliberately not observed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    primary: Option<FileRevision>,
    companions: Vec<Option<FileRevision>>,
}

fn has_extension(path: &Path, ext: &[&str]) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| ext.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// SQLite commits can change `-wal` or `-journal` without changing the main database. Unix
/// SQLite names them from the resolved database path, so commits through a link land beside the
/// target; Windows names them from the path as given. Both are observed when they differ.
fn sqlite_sidecars(path: &Path, noun: &str, out: &mut Vec<Option<FileRevision>>) -> Result<(), String> {
    let resolved = std::fs::canonicalize(path).ok().filter(|resolved| resolved != path);
    for base in std::iter::once(path).chain(resolved.as_deref()) {
        for suffix in ["-wal", "-journal"] {
            let mut sidecar = base.as_os_str().to_owned();
            sidecar.push(suffix);
            out.push(FileRevision::read(Path::new(&sidecar), Presence::Sidecar, noun)?);
        }
    }
    Ok(())
}

impl Observation {
    /// A local PMTiles or MBTiles archive (`file://` accepted): a missing file is an error.
    pub fn archive(path: &str) -> Result<Self, String> {
        let path = Path::new(path.strip_prefix("file://").unwrap_or(path));
        let primary = FileRevision::read(path, Presence::Required, "archive")?;
        let mut companions = Vec::new();
        if has_extension(path, &["mbtiles"]) {
            sqlite_sidecars(path, "archive", &mut companions)?;
        }
        Ok(Self { primary, companions })
    }

    /// Metadata only: safe for an archive's final landing, with no scan or decompression.
    pub fn unchanged_archive(&self, path: &str) -> Result<(), String> {
        if *self != Self::archive(path)? {
            return Err(format!("{path}: the archive changed while it was read; finish writing and retry the command"));
        }
        Ok(())
    }

    /// A monitored source at its linked path, with the companions its reader reads or sizes: a
    /// Shapefile's lowercase `.dbf`, `.shx` and `.prj` and every ASCII-case `.cpg`
    /// (`rustgis_data::dbf`), a MapInfo file's `.mid` (`rustgis_data::io`), a SQLite file's
    /// `-wal` and `-journal`, or a DuckDB database's `.wal` (commits not yet checkpointed, as the
    /// DuckDB conversion's cache key counts them). A missing source is
    /// [`Observation::missing`], not an error.
    pub fn source(path: &str) -> Result<Self, String> {
        let path = Path::new(path);
        let primary = FileRevision::read(path, Presence::Optional, "source")?;
        let mut companions = Vec::new();
        if has_extension(path, &["shp"]) {
            for ext in ["dbf", "shx", "prj", "cpg", "CPG", "Cpg", "cPg", "cpG", "CPg", "CpG", "cPG"] {
                companions.push(FileRevision::read(&path.with_extension(ext), Presence::Optional, "source")?);
            }
        } else if has_extension(path, &["mif"]) {
            // Its attributes: the spellings `rustgis_data::io` reads.
            for ext in ["mid", "MID", "Mid"] {
                companions.push(FileRevision::read(&path.with_extension(ext), Presence::Optional, "source")?);
            }
        } else if has_extension(path, &["gpkg", "mbtiles", "sqlite"]) {
            sqlite_sidecars(path, "source", &mut companions)?;
        } else if has_extension(path, &["duckdb", "ddb", "db"]) {
            // A `.db` may be either a DuckDB or a SQLite database: both logs are watched.
            if has_extension(path, &["db"]) {
                sqlite_sidecars(path, "source", &mut companions)?;
            }
            let mut wal = path.as_os_str().to_owned();
            wal.push(".wal");
            companions.push(FileRevision::read(Path::new(&wal), Presence::Sidecar, "source")?);
        }
        Ok(Self { primary, companions })
    }

    /// Nothing is at the linked path.
    pub fn missing(&self) -> bool {
        self.primary.is_none()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rustgis-observe-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn empty_sqlite_sidecars_match_absent_ones_but_commits_do_not() {
        let dir = dir("sidecars");
        // (file, its sidecars: watched or not)
        let cases: [(&str, &[(&str, bool)]); 4] = [
            ("clean.mbtiles", &[("-wal", true), ("-journal", true), ("-shm", false)]),
            ("clean.gpkg", &[("-wal", true), ("-journal", true), ("-shm", false)]),
            ("clean.duckdb", &[(".wal", true), ("-wal", false)]),
            // Either a DuckDB or a SQLite database.
            ("clean.db", &[(".wal", true), ("-wal", true), ("-journal", true), ("-shm", false)]),
        ];
        for (name, sidecars) in cases {
            let path = dir.join(name);
            std::fs::write(&path, b"main database").unwrap();
            let path = path.display().to_string();
            let observe = |p: &str| if name.ends_with("mbtiles") { Observation::archive(p) } else { Observation::source(p) };
            let observed = observe(&path).unwrap();
            for (sidecar, watched) in sidecars {
                let sidecar = format!("{path}{sidecar}");
                std::fs::write(&sidecar, b"").unwrap();
                assert_eq!(observe(&path).unwrap(), observed, "an empty sidecar created by a reader is not a commit");
                std::fs::write(&sidecar, b"committed frame").unwrap();
                assert_eq!(observe(&path).unwrap() != observed, *watched, "{sidecar}");
                std::fs::remove_file(&sidecar).unwrap();
                assert_eq!(observe(&path).unwrap(), observed);
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn linked_mbtiles_observes_the_sidecars_beside_its_target_and_beside_the_link() {
        let dir = dir("linked");
        let target = dir.join("target.mbtiles");
        std::fs::write(&target, b"main database").unwrap();
        let link = dir.join("link.mbtiles");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let link = link.display().to_string();
        let observed = Observation::archive(&link).unwrap();
        observed.unchanged_archive(&link).expect("nothing was committed");
        // SQLite on unix commits beside the target; on Windows beside the path as given.
        for beside in [target.display().to_string(), link.clone()] {
            for sidecar in ["-wal", "-journal"] {
                let sidecar = format!("{beside}{sidecar}");
                std::fs::write(&sidecar, b"committed frame").unwrap();
                assert!(observed.unchanged_archive(&link).is_err(), "a commit to {sidecar} escaped the freshness check");
                std::fs::remove_file(&sidecar).unwrap();
                observed.unchanged_archive(&link).unwrap();
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn shapefile_and_mapinfo_components_the_reader_opens_are_observed() {
        let dir = dir("components");
        for (main, exts) in [("roads.shp", &["dbf", "shx", "prj", "cpg", "CPG"][..]), ("roads.mif", &["mid"][..])] {
            let main = dir.join(main);
            std::fs::write(&main, b"shapes").unwrap();
            let path = main.display().to_string();
            let mut observed = Observation::source(&path).unwrap();
            assert!(!observed.missing());
            // Created and rewritten: every component the reader opens counts.
            for ext in exts {
                let component = main.with_extension(ext);
                std::fs::write(&component, b"1").unwrap();
                let created = Observation::source(&path).unwrap();
                assert_ne!(created, observed, "{ext} created");
                std::fs::write(&component, b"22").unwrap();
                let rewritten = Observation::source(&path).unwrap();
                assert_ne!(rewritten, created, "{ext} rewritten");
                observed = rewritten;
            }
            std::fs::remove_file(&main).unwrap();
            assert!(Observation::source(&path).unwrap().missing());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
