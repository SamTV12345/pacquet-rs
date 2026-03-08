use crate::{LinkFileError, link_file};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_npmrc::PackageImportMethod;
use rayon::prelude::*;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// Error type for [`create_cas_files`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum CreateCasFilesError {
    #[diagnostic(transparent)]
    LinkFile(#[error(source)] LinkFileError),
}

/// If `dir_path` doesn't exist, create and populate it with files from `cas_paths`.
///
/// If `dir_path` already exists, do nothing.
pub fn create_cas_files(
    import_method: PackageImportMethod,
    dir_path: &Path,
    cas_paths: &HashMap<String, PathBuf>,
) -> Result<(), CreateCasFilesError> {
    assert_eq!(
        import_method,
        PackageImportMethod::Auto,
        "Only PackageImportMethod::Auto is currently supported, but {dir_path:?} requires {import_method:?}",
    );

    cas_paths
        .par_iter()
        .try_for_each(|(cleaned_entry, store_path)| {
            link_file(store_path, &dir_path.join(cleaned_entry))
        })
        .map_err(CreateCasFilesError::LinkFile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write};
    use tempfile::tempdir;

    #[test]
    fn fills_missing_files_even_when_target_dir_already_exists() {
        let temp = tempdir().expect("create tempdir");
        let store = temp.path().join("store");
        let target = temp.path().join("target");
        fs::create_dir_all(&store).expect("create store dir");
        fs::create_dir_all(&target).expect("create target dir");

        let store_a = store.join("a.txt");
        let store_b = store.join("b.txt");
        fs::File::create(&store_a)
            .and_then(|mut file| file.write_all(b"a"))
            .expect("write store a");
        fs::File::create(&store_b)
            .and_then(|mut file| file.write_all(b"b"))
            .expect("write store b");

        // Simulate a partially-existing package directory (happens when previous cleanup failed).
        let target_a = target.join("a.txt");
        fs::copy(&store_a, &target_a).expect("seed existing file");

        let cas_paths =
            HashMap::from([("a.txt".to_string(), store_a), ("b.txt".to_string(), store_b)]);

        create_cas_files(PackageImportMethod::Auto, &target, &cas_paths).expect("create cas files");

        assert!(target.join("a.txt").is_file());
        assert!(target.join("b.txt").is_file());
    }
}
