//! Content identity for an existing local model, not a model registry/download API.
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

/// Hash supported model artifacts in stable relative-path order using 1 MiB RAM.
/// HF file symlinks are followed to existing regular files; directory symlinks are
/// not traversed. Absolute cache location and file timestamps are not identity.
pub fn model_fingerprint(directory: impl AsRef<Path>) -> Result<String> {
    let root = directory
        .as_ref()
        .canonicalize()
        .context("local model directory")?;
    ensure!(
        root.is_dir(),
        "model fingerprint requires a local directory"
    );
    let mut files = Vec::<PathBuf>::new();
    let mut directories = vec![root.clone()];
    let mut visited = 0usize;
    while let Some(directory) = directories.pop() {
        visited += 1;
        ensure!(visited < 10000, "model directory traversal limit exceeded");
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if kind.is_dir() {
                directories.push(path);
                ensure!(
                    directories.len() + files.len() < 10000,
                    "model directory exceeds artifact limit"
                );
                continue;
            }
            let name = entry.file_name();
            let name = name.to_str().context("model artifact path must be UTF-8")?;
            let included = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                matches!(e, "safetensors" | "json" | "model" | "tiktoken" | "jinja")
            }) || matches!(name, "vocab.txt" | "merges.txt" | "vocab" | "merges");
            if included {
                ensure!(
                    fs::metadata(&path)?.is_file(),
                    "model artifact is not an existing regular file: {}",
                    path.display()
                );
                files.push(path);
                ensure!(files.len() < 10000, "model artifact limit exceeded");
            }
        }
    }
    files.sort_by(|a, b| {
        a.strip_prefix(&root)
            .unwrap()
            .cmp(b.strip_prefix(&root).unwrap())
    });
    ensure!(
        files.iter().any(|p| p == &root.join("config.json"))
            && files
                .iter()
                .any(|p| p.extension().is_some_and(|e| e == "safetensors")),
        "model fingerprint requires config.json and safetensors weights"
    );
    let mut manifest = Sha256::new();
    manifest.update(b"forum-local-model-fingerprint-v1\0");
    let mut buffer = vec![0u8; 1024 * 1024];
    for path in files {
        let name = path
            .strip_prefix(&root)?
            .to_str()
            .context("model relative path must be UTF-8")?;
        let mut file =
            File::open(&path).with_context(|| format!("read model artifact {}", path.display()))?;
        let before = file.metadata()?;
        let mut hash = Sha256::new();
        let mut count = 0u64;
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
            count = count
                .checked_add(n as u64)
                .context("model file size overflow")?;
        }
        let after = file.metadata()?;
        ensure!(
            count == before.len()
                && after.len() == before.len()
                && after.modified()? == before.modified()?,
            "model artifact changed during fingerprinting: {}",
            path.display()
        );
        manifest.update((name.len() as u64).to_le_bytes());
        manifest.update(name.as_bytes());
        manifest.update(count.to_le_bytes());
        manifest.update(hash.finalize());
    }
    Ok(format!("sha256:{:x}", manifest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, DirBuilderExt};
    #[test]
    fn content_identity_is_location_independent_and_supports_hf_file_symlinks() {
        let root = std::env::temp_dir().join(format!("forum-fingerprint-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        fs::write(root.join("blob"), b"synthetic weight bytes").unwrap();
        for model in [&first, &second] {
            fs::write(model.join("config.json"), b"{}").unwrap();
            fs::write(model.join("tokenizer.json"), b"{\"vocab\":{}}").unwrap();
        }
        symlink(root.join("blob"), first.join("model.safetensors")).unwrap();
        fs::copy(root.join("blob"), second.join("model.safetensors")).unwrap();
        let original = model_fingerprint(&first).unwrap();
        assert_eq!(original, model_fingerprint(&second).unwrap());
        fs::write(second.join("README.md"), b"ignored documentation").unwrap();
        assert_eq!(original, model_fingerprint(&second).unwrap());
        fs::write(second.join("chat_template.jinja"), b"template version one").unwrap();
        let with_template = model_fingerprint(&second).unwrap();
        fs::write(second.join("chat_template.jinja"), b"template version two").unwrap();
        assert_ne!(with_template, model_fingerprint(&second).unwrap());
        fs::remove_file(second.join("chat_template.jinja")).unwrap();
        fs::write(second.join("model.safetensors"), b"different weight bytes").unwrap();
        assert_ne!(original, model_fingerprint(&second).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
}
