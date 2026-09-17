//! Read-only model selection. Download destinations never inherit inference overrides.
use std::{
    collections::HashMap,
    env, fs,
    path::{Component, Path, PathBuf},
};

pub const ASR_MODEL_ENV: &str = "QWEN3_ASR_MODEL_PATH";
pub const TRANSLATOR_MODEL_ENV: &str = "QWEN35_TRANSLATOR_MODEL_PATH";
const ASR_DIRECTORY: &str = "qwen3-asr-1.7b";
const TRANSLATOR_DIRECTORY: &str = "Qwen3.5-2B-MLX-4bit";

#[derive(Debug, PartialEq, Eq)]
pub struct ModelPaths {
    pub asr: PathBuf,
    pub translator: PathBuf,
    // The upstream ASR loader caches a generated tokenizer in the model folder.
    // Only the Forum-owned destination may use that fallback.
    asr_may_cache_tokenizer: bool,
}

impl ModelPaths {
    pub fn resolve_current() -> Result<Self, String> {
        let home = dirs::home_dir().ok_or("Could not resolve the macOS home directory")?;
        let selected = Self::resolve(
            &home,
            env::var(ASR_MODEL_ENV).ok().as_deref(),
            env::var(TRANSLATOR_MODEL_ENV).ok().as_deref(),
        );
        selected.validate_absolute()?;
        Ok(selected)
    }

    pub fn owned_current() -> Result<Self, String> {
        let home = dirs::home_dir().ok_or("Could not resolve the macOS home directory")?;
        Ok(Self::owned(&home))
    }

    pub fn validate_download_overrides() -> Result<(), String> {
        let home = dirs::home_dir().ok_or("Could not resolve the macOS home directory")?;
        Self::validate_overrides(
            &home,
            env::var(ASR_MODEL_ENV).ok().as_deref(),
            env::var(TRANSLATOR_MODEL_ENV).ok().as_deref(),
        )
    }

    fn validate_overrides(
        home: &Path,
        asr_override: Option<&str>,
        translator_override: Option<&str>,
    ) -> Result<(), String> {
        let selected = Self::resolve(home, asr_override, translator_override);
        for (key, explicit, valid) in [
            (
                ASR_MODEL_ENV,
                asr_override,
                selected.asr.is_absolute()
                    && asr_ready(&selected.asr, selected.asr_may_cache_tokenizer),
            ),
            (
                TRANSLATOR_MODEL_ENV,
                translator_override,
                selected.translator.is_absolute() && translator_ready(&selected.translator),
            ),
        ] {
            if explicit.is_some_and(|value| !value.trim().is_empty()) && !valid {
                return Err(format!("Please correct or remove {key}: the explicit inference model path is incomplete or is not absolute. Downloading Forum models will not replace this override."));
            }
        }
        Ok(())
    }

    fn owned(home: &Path) -> Self {
        let root = home
            .join("Library/Application Support")
            .join(crate::identity::DATA_DIRECTORY_NAME)
            .join("models");
        let asr = root.join(ASR_DIRECTORY);
        let asr_may_cache_tokenizer = has_no_symlink_ancestors(&asr);
        Self {
            asr,
            translator: root.join(TRANSLATOR_DIRECTORY),
            asr_may_cache_tokenizer,
        }
    }

    fn resolve(home: &Path, asr_override: Option<&str>, translator_override: Option<&str>) -> Self {
        let owned = Self::owned(home);
        let legacy = home.join(".OminiX/models");
        let asr_override = asr_override.filter(|value| !value.trim().is_empty());
        let translator_override = translator_override.filter(|value| !value.trim().is_empty());
        let (asr, asr_may_cache_tokenizer) = if let Some(explicit) = asr_override {
            // Preserve an explicit choice even when incomplete; readiness then fails
            // rather than silently loading a different model or downloading into it.
            let path = PathBuf::from(explicit);
            let may_cache = path == owned.asr && owned.asr_may_cache_tokenizer;
            (path, may_cache)
        } else if asr_ready(&owned.asr, owned.asr_may_cache_tokenizer) {
            (owned.asr, owned.asr_may_cache_tokenizer)
        } else if asr_ready(&legacy.join(ASR_DIRECTORY), false) {
            (legacy.join(ASR_DIRECTORY), false)
        } else {
            (owned.asr, owned.asr_may_cache_tokenizer)
        };
        let translator = if let Some(explicit) = translator_override {
            PathBuf::from(explicit)
        } else if translator_ready(&owned.translator) {
            owned.translator
        } else if translator_ready(&legacy.join(TRANSLATOR_DIRECTORY)) {
            legacy.join(TRANSLATOR_DIRECTORY)
        } else {
            owned.translator
        };
        Self {
            asr,
            translator,
            asr_may_cache_tokenizer,
        }
    }

    pub fn ready(&self) -> bool {
        self.validate_absolute().is_ok()
            && asr_ready(&self.asr, self.asr_may_cache_tokenizer)
            && translator_ready(&self.translator)
    }

    fn validate_absolute(&self) -> Result<(), String> {
        for (key, path) in [
            (ASR_MODEL_ENV, &self.asr),
            (TRANSLATOR_MODEL_ENV, &self.translator),
        ] {
            if !path.is_absolute() {
                return Err(format!("{key} must be an absolute model path; relative paths change meaning in the Dora runtime"));
            }
        }
        Ok(())
    }

    pub fn env_vars(&self) -> Result<HashMap<String, String>, String> {
        self.validate_absolute()?;
        [
            (ASR_MODEL_ENV, &self.asr),
            (TRANSLATOR_MODEL_ENV, &self.translator),
        ]
        .into_iter()
        .map(|(key, path)| {
            path.to_str()
                .map(|value| (key.to_string(), value.to_string()))
                .ok_or_else(|| format!("Model path is not valid Unicode: {}", path.display()))
        })
        .collect()
    }
}

fn has_no_symlink_ancestors(path: &Path) -> bool {
    // Do not let a user-created link turn an owned cache write into a write in
    // another application's model directory. This never creates the directory.
    let mut ancestor = PathBuf::new();
    for component in path.components() {
        ancestor.push(component);
        match fs::symlink_metadata(&ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => return false,
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return false,
        }
    }
    true
}

fn nonempty_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

fn weights_ready(directory: &Path) -> bool {
    if nonempty_file(&directory.join("model.safetensors")) {
        return true;
    }
    let Some(index) = fs::read(directory.join("model.safetensors.index.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    else {
        return false;
    };
    let Some(weights) = index
        .get("weight_map")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    !weights.is_empty()
        && weights.values().all(|value| {
            value.as_str().is_some_and(|name| {
                let path = Path::new(name);
                !name.is_empty()
                    && path
                        .components()
                        .all(|part| matches!(part, Component::Normal(_)))
                    && name.ends_with(".safetensors")
                    && nonempty_file(&directory.join(path))
            })
        })
}

fn asr_ready(directory: &Path, may_cache_tokenizer: bool) -> bool {
    nonempty_file(&directory.join("config.json"))
        && nonempty_file(&directory.join("tokenizer_config.json"))
        && weights_ready(directory)
        && (nonempty_file(&directory.join("tokenizer.json"))
            || (may_cache_tokenizer
                && nonempty_file(&directory.join("vocab.json"))
                && nonempty_file(&directory.join("merges.txt"))))
}

fn translator_ready(directory: &Path) -> bool {
    nonempty_file(&directory.join("config.json"))
        && nonempty_file(&directory.join("tokenizer_config.json"))
        && nonempty_file(&directory.join("tokenizer.json"))
        && weights_ready(directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path =
                env::temp_dir().join(format!("forum-model-selection-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
        fn complete(&self, directory: &Path) {
            fs::create_dir_all(directory).unwrap();
            for name in [
                "config.json",
                "tokenizer_config.json",
                "tokenizer.json",
                "model.safetensors",
            ] {
                fs::write(directory.join(name), b"fixture").unwrap();
            }
        }
        fn legacy(&self) -> ModelPaths {
            let root = self.0.join(".OminiX/models");
            ModelPaths {
                asr: root.join(ASR_DIRECTORY),
                translator: root.join(TRANSLATOR_DIRECTORY),
                asr_may_cache_tokenizer: false,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn snapshot(directory: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut files = Vec::new();
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.extend(snapshot(&path));
            } else {
                files.push((path.clone(), fs::read(path).unwrap()));
            }
        }
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    }

    #[test]
    fn selection_reuses_complete_legacy_without_writing_or_creating_owned_directory() {
        let fixture = Fixture::new();
        let legacy = fixture.legacy();
        fixture.complete(&legacy.asr);
        fixture.complete(&legacy.translator);
        let before = snapshot(&fixture.0);
        let selected = ModelPaths::resolve(&fixture.0, None, None);
        assert_eq!(selected, legacy);
        assert!(selected.ready());
        assert_eq!(
            selected.env_vars().unwrap()[ASR_MODEL_ENV],
            legacy.asr.to_str().unwrap()
        );
        assert_eq!(snapshot(&fixture.0), before);
        assert!(!ModelPaths::owned(&fixture.0).asr.exists());
    }

    #[test]
    fn explicit_inference_overrides_win_and_never_change_download_destinations() {
        let fixture = Fixture::new();
        let owned = ModelPaths::owned(&fixture.0);
        fixture.complete(&owned.asr);
        fixture.complete(&owned.translator);
        let asr = fixture.0.join("user-asr");
        let translator = fixture.0.join("user-translator");
        let selected = ModelPaths::resolve(&fixture.0, asr.to_str(), translator.to_str());
        assert_eq!(selected.asr, asr);
        assert_eq!(selected.translator, translator);
        assert!(!selected.ready());
        assert_eq!(ModelPaths::owned(&fixture.0), owned);
        assert_eq!(ModelPaths::resolve(&fixture.0, Some(" "), Some("")), owned);
    }

    #[test]
    fn complete_owned_wins_but_partial_owned_falls_back_per_model() {
        let fixture = Fixture::new();
        let owned = ModelPaths::owned(&fixture.0);
        let legacy = fixture.legacy();
        for path in [
            &owned.asr,
            &owned.translator,
            &legacy.asr,
            &legacy.translator,
        ] {
            fixture.complete(path);
        }
        assert_eq!(ModelPaths::resolve(&fixture.0, None, None), owned);
        fs::remove_file(owned.asr.join("model.safetensors")).unwrap();
        let before = snapshot(&fixture.0);
        let selected = ModelPaths::resolve(&fixture.0, None, None);
        assert_eq!(selected.asr, legacy.asr);
        assert_eq!(selected.translator, owned.translator);
        assert!(selected.ready());
        assert_eq!(snapshot(&fixture.0), before);
    }

    #[test]
    fn missing_models_choose_owned_without_creating_files() {
        let fixture = Fixture::new();
        let selected = ModelPaths::resolve(&fixture.0, None, None);
        assert_eq!(selected, ModelPaths::owned(&fixture.0));
        assert!(!selected.ready());
        assert!(snapshot(&fixture.0).is_empty());
    }

    #[test]
    fn shared_asr_requires_cached_tokenizer_to_avoid_upstream_cache_write() {
        let fixture = Fixture::new();
        let legacy = fixture.legacy();
        fixture.complete(&legacy.asr);
        fs::remove_file(legacy.asr.join("tokenizer.json")).unwrap();
        fs::write(legacy.asr.join("vocab.json"), b"{}").unwrap();
        fs::write(legacy.asr.join("merges.txt"), b"merges").unwrap();
        assert!(asr_ready(&legacy.asr, true));
        assert!(!asr_ready(&legacy.asr, false));
        assert_eq!(
            ModelPaths::resolve(&fixture.0, None, None).asr,
            ModelPaths::owned(&fixture.0).asr
        );
    }

    #[test]
    fn sharded_weights_require_every_nonempty_local_shard() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("model.safetensors.index.json"), br#"{"weight_map":{"a":"model-00001-of-00002.safetensors","b":"model-00002-of-00002.safetensors"}}"#).unwrap();
        fs::write(
            fixture.0.join("model-00001-of-00002.safetensors"),
            b"weight",
        )
        .unwrap();
        assert!(!weights_ready(&fixture.0));
        fs::write(fixture.0.join("model-00002-of-00002.safetensors"), b"").unwrap();
        assert!(!weights_ready(&fixture.0));
        fs::write(
            fixture.0.join("model-00002-of-00002.safetensors"),
            b"weight",
        )
        .unwrap();
        assert!(weights_ready(&fixture.0));
        fs::write(
            fixture.0.join("model.safetensors.index.json"),
            br#"{"weight_map":{"a":"../outside.safetensors"}}"#,
        )
        .unwrap();
        assert!(!weights_ready(&fixture.0));
    }

    #[test]
    fn relative_overrides_are_rejected_before_readiness_or_runtime_environment() {
        let fixture = Fixture::new();
        let selected = ModelPaths::resolve(
            &fixture.0,
            Some("relative/asr"),
            Some("relative/translator"),
        );
        assert!(!selected.ready());
        assert!(selected
            .validate_absolute()
            .unwrap_err()
            .contains("absolute"));
        assert!(selected.env_vars().is_err());
        assert!(snapshot(&fixture.0).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn owned_symlink_never_allows_asr_cache_writes_to_external_directory() {
        let fixture = Fixture::new();
        let owned = ModelPaths::owned(&fixture.0);
        let legacy = fixture.legacy();
        fixture.complete(&legacy.asr);
        fixture.complete(&legacy.translator);
        fs::remove_file(legacy.asr.join("tokenizer.json")).unwrap();
        fs::write(legacy.asr.join("vocab.json"), b"{}").unwrap();
        fs::write(legacy.asr.join("merges.txt"), b"merges").unwrap();
        fs::create_dir_all(owned.asr.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&legacy.asr, &owned.asr).unwrap();
        let before = snapshot(&fixture.0);
        for selected in [
            ModelPaths::resolve(&fixture.0, None, None),
            ModelPaths::resolve(&fixture.0, owned.asr.to_str(), None),
        ] {
            assert!(!selected.asr_may_cache_tokenizer);
            assert!(!selected.ready());
        }
        assert_eq!(snapshot(&fixture.0), before);
        fs::write(legacy.asr.join("tokenizer.json"), b"cached").unwrap();
        let selected = ModelPaths::resolve(&fixture.0, None, None);
        assert!(selected.ready());
        assert!(!selected.asr_may_cache_tokenizer);
    }

    #[test]
    fn invalid_explicit_override_blocks_download_and_names_the_variable() {
        let fixture = Fixture::new();
        let missing_asr = fixture.0.join("external-asr");
        let missing_translator = fixture.0.join("external-translator");
        assert!(
            ModelPaths::validate_overrides(&fixture.0, missing_asr.to_str(), None)
                .unwrap_err()
                .contains(ASR_MODEL_ENV)
        );
        assert!(
            ModelPaths::validate_overrides(&fixture.0, None, missing_translator.to_str())
                .unwrap_err()
                .contains(TRANSLATOR_MODEL_ENV)
        );
        assert!(ModelPaths::validate_overrides(&fixture.0, Some("relative/asr"), None).is_err());
        assert!(snapshot(&fixture.0).is_empty());
        fixture.complete(&missing_asr);
        let before = snapshot(&fixture.0);
        assert!(ModelPaths::validate_overrides(&fixture.0, missing_asr.to_str(), None).is_ok());
        assert!(ModelPaths::validate_overrides(&fixture.0, Some(" "), Some("")).is_ok());
        assert_eq!(snapshot(&fixture.0), before);
    }
}
