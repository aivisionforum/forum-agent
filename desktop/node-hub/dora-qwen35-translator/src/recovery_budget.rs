//! A manual recovery action grants a durable, finite number of extra attempts.
//! Restarting the same child process must never refresh that allowance.
use anyhow::{ensure, Context, Result};
use forum_contracts::{TranslationRequested, Uuid};
use forum_runtime::RuntimeConfig;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
};

pub struct RecoveryBudget {
    directory: PathBuf,
    session_id: Uuid,
    action_id: Option<Uuid>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Allowance {
    session_id: Uuid,
    action_id: Uuid,
    translation_id: Uuid,
    initial_attempt: u32,
    ceiling: u32,
}

impl RecoveryBudget {
    pub fn from_env(config: &RuntimeConfig) -> Result<Self> {
        let action_id = if std::env::var("FORUM_RETRY_EXHAUSTED").as_deref() == Ok("1") {
            let action: Uuid = std::env::var("FORUM_TRANSLATOR_RECOVERY_ID")
                .context("explicit translation recovery requires a stable recovery action ID")?
                .parse()?;
            ensure!(!action.is_nil(), "nil recovery action ID");
            Some(action)
        } else {
            None
        };
        Self::for_action(config, action_id)
    }

    pub(crate) fn for_action(config: &RuntimeConfig, action_id: Option<Uuid>) -> Result<Self> {
        ensure!(
            action_id.is_none_or(|id| !id.is_nil()),
            "nil recovery action ID"
        );
        Ok(Self {
            directory: config.producer_dir.clone(),
            session_id: config.session.session_id,
            action_id,
        })
    }

    pub fn ceiling(&self, request: &TranslationRequested) -> Result<u32> {
        let Some(action_id) = self.action_id else {
            return Ok(3);
        };
        let path = self.directory.join(format!(
            "retry-{action_id}-{}.budget",
            request.translation_id
        ));
        if path.try_exists()? {
            let mut file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)?;
            ensure!(
                file.metadata()?.is_file() && file.metadata()?.len() <= 1024,
                "invalid recovery allowance file"
            );
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            let allowance: Allowance = serde_json::from_slice(&bytes)?;
            ensure!(
                allowance.session_id == self.session_id
                    && allowance.action_id == action_id
                    && allowance.translation_id == request.translation_id
                    && allowance.initial_attempt.checked_add(3) == Some(allowance.ceiling),
                "recovery allowance scope mismatch"
            );
            return Ok(allowance.ceiling);
        }
        let ceiling = request
            .attempt
            .checked_add(3)
            .context("translation attempt exhausted")?;
        let allowance = Allowance {
            session_id: self.session_id,
            action_id,
            translation_id: request.translation_id,
            initial_attempt: request.attempt,
            ceiling,
        };
        // .budget files are deliberately not producer event .json files.
        let temporary = self.directory.join(format!("retry-{}.tmp", Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temporary)?;
            file.write_all(&serde_json::to_vec(&allowance)?)?;
            file.sync_all()?;
            fs::rename(&temporary, &path)?;
            fs::File::open(&self.directory)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        Ok(ceiling)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forum_contracts::{Revision, SourceSpan};
    fn request() -> TranslationRequested {
        TranslationRequested {
            translation_id: Uuid::new_v4(),
            revision: Revision::FIRST,
            attempt: 3,
            target_language: "en".into(),
            direction_epoch: 1,
            context_spans: vec![],
            source_spans: vec![SourceSpan {
                segment_id: Uuid::new_v4(),
                segment_revision: Revision::FIRST,
                start_utf8: 0,
                end_utf8: 3,
                quote: "不".into(),
            }],
            input_text: "不".into(),
            normalization_version: "identity-v1".into(),
            backend: "test".into(),
            model_manifest_id: "test".into(),
        }
    }
    #[test]
    fn manual_recovery_budget_survives_restarts_without_refreshing() {
        let directory = std::env::temp_dir().join(format!("f04-budget-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let session_id = Uuid::new_v4();
        let action_id = Uuid::new_v4();
        let mut request = request();
        let first = RecoveryBudget {
            directory: directory.clone(),
            session_id,
            action_id: Some(action_id),
        };
        assert_eq!(first.ceiling(&request).unwrap(), 6);
        drop(first);
        request.attempt = 6;
        let restarted = RecoveryBudget {
            directory: directory.clone(),
            session_id,
            action_id: Some(action_id),
        };
        assert_eq!(restarted.ceiling(&request).unwrap(), 6);
        let ordinary = RecoveryBudget {
            directory: directory.clone(),
            session_id,
            action_id: None,
        };
        assert_eq!(ordinary.ceiling(&request).unwrap(), 3);
        let new_user_action = RecoveryBudget {
            directory: directory.clone(),
            session_id,
            action_id: Some(Uuid::new_v4()),
        };
        assert_eq!(new_user_action.ceiling(&request).unwrap(), 9);
        fs::remove_dir_all(directory).unwrap();
    }
}
