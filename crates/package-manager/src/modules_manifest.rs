use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MODULES_MANIFEST_FILE_NAME: &str = ".modules.yaml";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModulesManifest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pruned_at: Option<String>,
}

pub(crate) fn should_prune_orphaned_virtual_store_entries(
    modules_dir: &Path,
    modules_cache_max_age_minutes: u64,
) -> bool {
    let Some(pruned_at) =
        read_modules_manifest(modules_dir).and_then(|manifest| manifest.pruned_at)
    else {
        return true;
    };

    if modules_cache_max_age_minutes == 0 {
        return true;
    }

    cache_expired(&pruned_at, modules_cache_max_age_minutes)
}

pub(crate) fn write_modules_manifest_pruned_at(modules_dir: &Path) -> io::Result<()> {
    fs::create_dir_all(modules_dir)?;
    let manifest = ModulesManifest {
        pruned_at: Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::from_secs(0))
                .as_secs()
                .to_string(),
        ),
    };
    let yaml = serde_yaml::to_string(&manifest).map_err(io::Error::other)?;
    fs::write(modules_dir.join(MODULES_MANIFEST_FILE_NAME), yaml)
}

fn read_modules_manifest(modules_dir: &Path) -> Option<ModulesManifest> {
    let content = fs::read_to_string(modules_dir.join(MODULES_MANIFEST_FILE_NAME)).ok()?;
    serde_yaml::from_str(&content).ok()
}

fn cache_expired(pruned_at: &str, modules_cache_max_age_minutes: u64) -> bool {
    let Ok(pruned_at_epoch_secs) = pruned_at.parse::<u64>() else {
        return true;
    };
    let Ok(elapsed) =
        SystemTime::now().duration_since(UNIX_EPOCH + Duration::from_secs(pruned_at_epoch_secs))
    else {
        return false;
    };
    elapsed >= Duration::from_secs(modules_cache_max_age_minutes.saturating_mul(60))
}

#[cfg(test)]
mod tests {
    use super::{
        MODULES_MANIFEST_FILE_NAME, should_prune_orphaned_virtual_store_entries,
        write_modules_manifest_pruned_at,
    };
    use std::{
        fs,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn should_prune_without_modules_manifest() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(should_prune_orphaned_virtual_store_entries(dir.path(), 10));
    }

    #[test]
    fn should_not_prune_when_manifest_is_fresh() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_modules_manifest_pruned_at(dir.path()).expect("write modules manifest");

        assert!(!should_prune_orphaned_virtual_store_entries(dir.path(), 10));
    }

    #[test]
    fn should_prune_when_manifest_is_expired() {
        let dir = tempfile::tempdir().expect("tempdir");
        let old = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("unix time")
            .saturating_sub(Duration::from_secs(3 * 60))
            .as_secs();
        fs::write(dir.path().join(MODULES_MANIFEST_FILE_NAME), format!("prunedAt: '{old}'\n"))
            .expect("write modules manifest");

        assert!(should_prune_orphaned_virtual_store_entries(dir.path(), 2));
    }

    #[test]
    fn zero_cache_age_prunes_even_when_manifest_is_fresh() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_modules_manifest_pruned_at(dir.path()).expect("write modules manifest");

        assert!(should_prune_orphaned_virtual_store_entries(dir.path(), 0));
    }
}
