//! Versioned SQLite store and bounded single-writer actor for local meetings.
//! No Tauri, MLX, audio device, or network access; CoreHandle owns its writer thread.

use forum_contracts::*;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions, TryLockError},
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;
mod actor;
mod analysis;
mod legacy;

mod direction;
use direction::*;
mod reliable;
mod translation;
pub use actor::*;
pub use reliable::*;
pub use translation::*;

pub const DATABASE_VERSION: u32 = 5;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("database is already owned by another core process")]
    AlreadyOwned,
    #[error("core request queue is full")]
    QueueFull,
    #[error("core actor has stopped")]
    ActorClosed,
    #[error("core actor request panicked")]
    ActorPanicked,
    #[error("core acknowledgement deadline elapsed; commit outcome is unknown; replay the identical message")]
    AckUnknown,
    #[error("session state does not permit this operation")]
    InvalidState,
    #[error("translation result belongs to an outdated revision, attempt, or direction")]
    LateResult,
    #[error("requested source range is already owned or is not pending")]
    CoverageConflict,
    #[error("capture/producer seal does not reconcile with durable data")]
    SealMismatch,
    #[error("snapshot cursor is invalid or no longer available")]
    InvalidCursor,
    #[error(transparent)]
    Validation(#[from] ValidationError),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("database schema {0} is newer than this build")]
    UnsupportedDatabase(u32),
    #[error("event ID or producer sequence conflicts with an existing event")]
    EventIdConflict,
    #[error("entity ID conflicts with an existing entity")]
    EntityConflict,
    #[error("event, room, session or track ownership does not match")]
    ScopeMismatch,
    #[error("required entity not found: {0}")]
    NotFound(&'static str),
    #[error("capture or source revision has not been durably registered")]
    DependencyNotReady,
    #[error("expected revision {expected}; current revision is {actual:?}")]
    RevisionConflict { expected: u32, actual: Option<u32> },
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Receipt {
    pub message_id: Uuid,
    pub store_seq: u64,
    pub duplicate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RevisionOrigin {
    Asr,
    LegacyImport {
        file_sha256: String,
        line: u32,
        speaker_label: Option<String>,
    },
    Human {
        operator_id: String,
        reason: String,
        base_revision: Revision,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TranscriptRecord {
    pub session_id: Uuid,
    /// For a human revision, backend/model fields retain the original ASR
    /// provenance; `origin` identifies who produced this revision's text.
    pub payload: TranscriptFinal,
    pub origin: RevisionOrigin,
    pub created_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IncompleteSegment {
    pub segment_id: Uuid,
    pub track_id: Uuid,
    pub audio: AudioRange,
    pub recording_ref: Option<String>,
    /// None means capture registered but no ASR result has been committed.
    pub status: Option<TranscriptStatus>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionSnapshot {
    pub session: SessionSpec,
    pub cursor: u64,
    pub transcript: Vec<TranscriptRecord>,
    pub incomplete: Vec<IncompleteSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CoverageIntent {
    pub segment_id: Uuid,
    pub segment_revision: Revision,
    pub target_language: String,
    pub direction_epoch: u64,
    pub start_utf8: usize,
    pub end_utf8: usize,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OutboxMessage {
    pub store_seq: u64,
    pub message_id: Uuid,
    pub session_id: Uuid,
    pub event_type: String,
    /// Internal event data only. Never forward this as a public projection.
    pub body: serde_json::Value,
}

pub struct Store {
    connection: Connection,
    // Sidecar lock is shared by canonical path aliases; hard links are rejected.
    _ownership: Option<File>,
    pub migration_backup: Option<PathBuf>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        // A separate lock inode is essential: macOS SQLite locking conflicts
        // with flock on the database file itself. Resolve symlink aliases and
        // reject hard links, which otherwise could name different sidecars.
        drop(
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)?,
        );
        let path = path.canonicalize()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if path.metadata()?.nlink() != 1 {
                return Err(ValidationError::Invalid("database_hard_link").into());
            }
        }
        let lock_path = path.with_file_name(format!(
            "{}.core-owner",
            path.file_name()
                .ok_or(ValidationError::Invalid("database_path"))?
                .to_string_lossy()
        ));
        if std::fs::symlink_metadata(&lock_path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(ValidationError::Invalid("ownership_symlink").into());
        }
        let ownership = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        match ownership.try_lock() {
            Ok(()) => (),
            Err(TryLockError::WouldBlock) => return Err(StoreError::AlreadyOwned),
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
        let mut store =
            Self::from_connection(Connection::open(&path)?, Some(&path), Some(ownership))?;
        store.recover_interrupted_sessions()?;
        store.recover_analysis_jobs()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?, None, None)
    }

    fn from_connection(
        mut connection: Connection,
        path: Option<&Path>,
        ownership: Option<File>,
    ) -> Result<Self> {
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > DATABASE_VERSION {
            return Err(StoreError::UnsupportedDatabase(version));
        }
        connection.busy_timeout(Duration::from_secs(3))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        let mut migration_backup = None;
        if version > 0 && version < DATABASE_VERSION {
            if let Some(path) = path {
                let backup =
                    path.with_extension(format!("backup-v{version}-{}.sqlite", Uuid::new_v4()));
                connection.execute("VACUUM INTO ?1", [backup.to_string_lossy().as_ref()])?;
                File::open(&backup)?.sync_all()?;
                migration_backup = Some(backup);
            }
        }
        if version == 0 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(include_str!("../migrations/001_initial.sql"))?;
            tx.commit()?;
        }
        if version < 2 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(include_str!("../migrations/002_reliable.sql"))?;
            tx.commit()?;
        }
        if version < 3 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(include_str!("../migrations/003_recovery_history.sql"))?;
            tx.commit()?;
        }
        if version < 4 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(include_str!("../migrations/004_direction_boundaries.sql"))?;
            tx.commit()?;
        }
        if version < 5 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(include_str!("../migrations/005_analysis.sql"))?;
            tx.commit()?;
        }
        Ok(Self {
            connection,
            _ownership: ownership,
            migration_backup,
        })
    }

    /// Idempotent catalog setup, before capture begins. Future lifecycle
    /// commands will create their own versioned state events.
    pub fn create_session(&mut self, spec: &SessionSpec) -> Result<()> {
        spec.validate()?;
        let body = serde_json::to_string(spec)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT spec_json FROM sessions WHERE id=?1",
                [spec.session_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing != body {
                return Err(StoreError::EntityConflict);
            }
            return Ok(());
        }
        tx.execute(
            "INSERT OR IGNORE INTO events(id) VALUES(?1)",
            [spec.event_id.to_string()],
        )?;
        let room_event: Option<String> = tx
            .query_row(
                "SELECT event_id FROM rooms WHERE id=?1",
                [spec.room_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if room_event
            .as_deref()
            .is_some_and(|id| id != spec.event_id.to_string())
        {
            return Err(StoreError::ScopeMismatch);
        }
        tx.execute(
            "INSERT OR IGNORE INTO rooms(id,event_id) VALUES(?1,?2)",
            params![spec.room_id.to_string(), spec.event_id.to_string()],
        )?;
        tx.execute("INSERT INTO sessions(id,event_id,room_id,owner_device_id,title,spec_json) VALUES(?1,?2,?3,?4,?5,?6)",
            params![spec.session_id.to_string(), spec.event_id.to_string(), spec.room_id.to_string(),
                    spec.owner_device_id.to_string(), spec.title, body])?;
        tx.commit()?;
        Ok(())
    }

    pub fn create_track(&mut self, spec: &TrackSpec) -> Result<()> {
        spec.validate()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_session(&tx, spec.session_id)?;
        let body = serde_json::to_string(spec)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT spec_json FROM tracks WHERE id=?1",
                [spec.track_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing != body {
                return Err(StoreError::EntityConflict);
            }
            return Ok(());
        }
        tx.execute(
            "INSERT INTO tracks(id,session_id,sample_rate,spec_json) VALUES(?1,?2,?3,?4)",
            params![
                spec.track_id.to_string(),
                spec.session_id.to_string(),
                spec.sample_rate,
                body
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn register_capture(&mut self, event: &Event<CaptureSegmentClosed>) -> Result<Receipt> {
        event.validate_envelope(EventType::AudioSegmentClosed)?;
        event.payload.validate()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (receipt, body) = register_event(&tx, event)?;
        if receipt.duplicate {
            return Ok(receipt);
        }
        require_capture_state(&tx, event.session_id)?;
        let p = &event.payload;
        let track: Option<(String, u32)> = tx
            .query_row(
                "SELECT session_id,sample_rate FROM tracks WHERE id=?1",
                [p.track_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match track {
            None => return Err(StoreError::NotFound("track")),
            Some((session, rate))
                if session != event.session_id.to_string() || rate != p.audio.sample_rate =>
            {
                return Err(StoreError::ScopeMismatch);
            }
            _ => {}
        }
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM segments WHERE id=?1)",
            [p.segment_id.to_string()],
            |row| row.get(0),
        )?;
        if exists {
            return Err(StoreError::EntityConflict);
        }
        tx.execute(
            "INSERT INTO segments(id,session_id,track_id) VALUES(?1,?2,?3)",
            params![
                p.segment_id.to_string(),
                event.session_id.to_string(),
                p.track_id.to_string()
            ],
        )?;
        let (epoch, configured, targets) =
            capture_direction_at(&tx, event.session_id, p.track_id, &p.audio)?;
        tx.execute("INSERT INTO capture_segments(segment_id,audio_json,recording_ref,created_seq,direction_epoch,configured_source_language,targets_json) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![p.segment_id.to_string(), serde_json::to_string(&p.audio)?, p.recording_ref, receipt.store_seq,epoch,configured,targets])?;
        refresh_integrity(&tx, event.session_id)?;
        save_history(&tx, event.session_id, receipt.store_seq)?;
        append_outbox(&tx, event, &receipt, &body)?;
        tx.commit()?;
        Ok(receipt)
    }

    pub fn ingest_final(&mut self, event: &Event<TranscriptFinal>) -> Result<Receipt> {
        event.validate_envelope(EventType::TranscriptFinal)?;
        event.payload.validate()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (receipt, body) = register_event(&tx, event)?;
        if receipt.duplicate {
            return Ok(receipt);
        }
        let p = &event.payload;
        let (track_id, audio, current) = capture_context(&tx, event.session_id, p.segment_id)?;
        validate_final_direction(&tx, event.session_id, p)?;
        if track_id != p.track_id || audio != p.audio {
            return Err(StoreError::ScopeMismatch);
        }
        if let Some(current_revision) = current {
            let previous = load_revision(
                &tx,
                event.session_id,
                p.segment_id,
                current_revision.try_into()?,
            )?;
            if previous.payload.status != TranscriptStatus::Failed
                || previous.origin != RevisionOrigin::Asr
                || previous.payload.revision.next()? != p.revision
            {
                return Err(StoreError::RevisionConflict {
                    expected: current_revision.saturating_add(1),
                    actual: current,
                });
            }
            invalidate_source_translations(
                &tx,
                event.session_id,
                p.segment_id,
                previous.payload.revision,
                receipt.store_seq,
            )?;
        } else if p.revision != Revision::FIRST {
            return Err(StoreError::RevisionConflict {
                expected: 1,
                actual: current,
            });
        }
        let record = TranscriptRecord {
            session_id: event.session_id,
            payload: p.clone(),
            origin: RevisionOrigin::Asr,
            created_seq: receipt.store_seq,
        };
        insert_revision(&tx, &record)?;
        analysis::invalidate_analysis_source(&tx, record.payload.segment_id)?;
        tx.execute(
            "UPDATE capture_segments SET result_seq=?1 WHERE segment_id=?2",
            params![receipt.store_seq, p.segment_id.to_string()],
        )?;
        insert_coverage(&tx, &record)?;
        refresh_integrity(&tx, event.session_id)?;
        save_history(&tx, event.session_id, receipt.store_seq)?;
        append_outbox(&tx, event, &receipt, &body)?;
        tx.commit()?;
        Ok(receipt)
    }

    pub fn revise_transcript(&mut self, event: &Event<TranscriptRevision>) -> Result<Receipt> {
        event.validate_envelope(EventType::TranscriptRevised)?;
        event.payload.validate()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (receipt, body) = register_event(&tx, event)?;
        if receipt.duplicate {
            return Ok(receipt);
        }
        let p = &event.payload;
        let (_, _, current) = capture_context(&tx, event.session_id, p.segment_id)?;
        if current != Some(p.expected_revision.get()) {
            return Err(StoreError::RevisionConflict {
                expected: p.expected_revision.get(),
                actual: current,
            });
        }
        let mut record = load_revision(&tx, event.session_id, p.segment_id, p.expected_revision)?;
        record.payload.revision = p.expected_revision.next()?;
        record.payload.text = p.text.clone();
        record.payload.status = TranscriptStatus::Success;
        record.payload.reason = None;
        record.origin = RevisionOrigin::Human {
            operator_id: p.operator_id.clone(),
            reason: p.reason.clone(),
            base_revision: p.expected_revision,
        };
        record.created_seq = receipt.store_seq;
        insert_revision(&tx, &record)?;
        analysis::invalidate_analysis_source(&tx, record.payload.segment_id)?;
        tx.execute("UPDATE translation_coverage SET state='stale' WHERE segment_id=?1 AND segment_revision=?2",
            params![p.segment_id.to_string(), p.expected_revision.get()])?;
        invalidate_source_translations(
            &tx,
            event.session_id,
            p.segment_id,
            p.expected_revision,
            receipt.store_seq,
        )?;
        insert_coverage(&tx, &record)?;
        refresh_integrity(&tx, event.session_id)?;
        save_history(&tx, event.session_id, receipt.store_seq)?;
        append_outbox(&tx, event, &receipt, &body)?;
        tx.commit()?;
        Ok(receipt)
    }

    pub fn transcript_revision(
        &self,
        session_id: Uuid,
        segment_id: Uuid,
        revision: Revision,
    ) -> Result<TranscriptRecord> {
        load_revision(&self.connection, session_id, segment_id, revision)
    }

    /// Resolve an exact historical revision. Currentness is a separate check
    /// for future analysis/publication commits; this does not approve a quote.
    pub fn resolve_span(&self, session_id: Uuid, span: &SourceSpan) -> Result<String> {
        let record =
            self.transcript_revision(session_id, span.segment_id, span.segment_revision)?;
        if record.payload.status != TranscriptStatus::Success {
            return Err(StoreError::DependencyNotReady);
        }
        Ok(span.validate_against(&record.payload.text)?.to_owned())
    }

    pub fn session_snapshot(&mut self, session_id: Uuid) -> Result<SessionSnapshot> {
        // The initial SELECT fixes the WAL read view. Cursor and records below
        // therefore describe the same commit boundary, even with another reader.
        let tx = self.connection.transaction()?;
        let cursor: u64 = tx.query_row(
            "SELECT COALESCE(MAX(store_seq),0) FROM ingested_events",
            [],
            |row| row.get(0),
        )?;
        let session = require_session(&tx, session_id)?;
        let mut transcript = Vec::new();
        let mut incomplete = Vec::new();
        {
            let mut statement = tx.prepare(
                "SELECT s.id,s.track_id,c.audio_json,c.recording_ref,r.record_json
                FROM segments s JOIN capture_segments c ON c.segment_id=s.id
                LEFT JOIN segment_revisions r ON r.segment_id=s.id AND r.revision=s.current_revision
                WHERE s.session_id=?1",
            )?;
            let rows = statement.query_map([session_id.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?;
            for row in rows {
                let (segment, track, audio, recording_ref, record) = row?;
                let record: Option<TranscriptRecord> =
                    record.map(|body| serde_json::from_str(&body)).transpose()?;
                if let Some(record) = record
                    .as_ref()
                    .filter(|r| r.payload.status == TranscriptStatus::Success)
                {
                    transcript.push(record.clone());
                } else {
                    incomplete.push(IncompleteSegment {
                        segment_id: parse_uuid(&segment)?,
                        track_id: parse_uuid(&track)?,
                        audio: serde_json::from_str(&audio)?,
                        recording_ref,
                        status: record.as_ref().map(|r| r.payload.status),
                        reason: record.and_then(|r| r.payload.reason),
                    });
                }
            }
        }
        transcript.sort_by_key(|r| {
            (
                r.payload.audio.start_ms,
                r.payload.track_id,
                r.payload.segment_id,
            )
        });
        incomplete.sort_by_key(|r| (r.audio.start_ms, r.track_id, r.segment_id));
        tx.commit()?;
        Ok(SessionSnapshot {
            session,
            cursor,
            transcript,
            incomplete,
        })
    }

    pub fn coverage(&self, session_id: Uuid) -> Result<Vec<CoverageIntent>> {
        require_session(&self.connection, session_id)?;
        let mut statement = self.connection.prepare(
            "SELECT c.segment_id,c.segment_revision,c.target_language,
            c.direction_epoch,c.start_utf8,c.end_utf8,c.state FROM translation_coverage c
            JOIN segments s ON s.id=c.segment_id WHERE s.session_id=?1
            ORDER BY c.segment_id,c.segment_revision,c.target_language",
        )?;
        let rows = statement.query_map([session_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, u64>(3)?,
                row.get::<_, usize>(4)?,
                row.get::<_, usize>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        rows.map(|row| {
            let (segment, revision, target_language, direction_epoch, start_utf8, end_utf8, state) =
                row?;
            Ok(CoverageIntent {
                segment_id: parse_uuid(&segment)?,
                segment_revision: revision.try_into()?,
                target_language,
                direction_epoch,
                start_utf8,
                end_utf8,
                state,
            })
        })
        .collect()
    }

    pub fn outbox_after(&self, after: u64, limit: u32) -> Result<Vec<OutboxMessage>> {
        if after > i64::MAX as u64 || limit == 0 || limit > 1000 {
            return Err(ValidationError::Invalid("outbox_cursor_or_limit").into());
        }
        let mut statement = self.connection.prepare(
            "SELECT store_seq,message_id,session_id,event_type,body_json
            FROM outbox WHERE store_seq>?1 ORDER BY store_seq LIMIT ?2",
        )?;
        let rows = statement.query_map(params![after, limit], |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (store_seq, message_id, session_id, event_type, body) = row?;
            Ok(OutboxMessage {
                store_seq,
                message_id: parse_uuid(&message_id)?,
                session_id: parse_uuid(&session_id)?,
                event_type,
                body: serde_json::from_str(&body)?,
            })
        })
        .collect()
    }
}

fn parse_uuid(value: &str) -> Result<Uuid> {
    Uuid::parse_str(value).map_err(|_| ValidationError::Invalid("stored_uuid").into())
}

fn require_session(connection: &Connection, session_id: Uuid) -> Result<SessionSpec> {
    let body: Option<String> = connection
        .query_row(
            "SELECT spec_json FROM sessions WHERE id=?1",
            [session_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(serde_json::from_str(
        &body.ok_or(StoreError::NotFound("session"))?,
    )?)
}

fn register_event<T: Serialize>(
    tx: &Transaction<'_>,
    event: &Event<T>,
) -> Result<(Receipt, String)> {
    let session = require_session(tx, event.session_id)?;
    if session.event_id != event.event_id || session.room_id != event.room_id {
        return Err(StoreError::ScopeMismatch);
    }
    // Hash the canonical typed JSON, not incidental wire whitespace. Replays
    // must retain message ID, producer run/seq and all supported fields.
    let body = serde_json::to_string(event)?;
    let hash = format!("{:x}", Sha256::digest(body.as_bytes()));
    let mut statement = tx.prepare(
        "SELECT store_seq,message_id,producer_run_id,producer_seq,body_sha256
        FROM ingested_events WHERE message_id=?1 OR (producer_run_id=?2 AND producer_seq=?3)",
    )?;
    let existing = statement
        .query_map(
            params![
                event.message_id.to_string(),
                event.producer.run_id.to_string(),
                event.producer.seq
            ],
            |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if !existing.is_empty() {
        let (seq, message, run, producer_seq, existing_hash) = &existing[0];
        if existing.len() != 1
            || message != &event.message_id.to_string()
            || run != &event.producer.run_id.to_string()
            || *producer_seq != event.producer.seq
            || existing_hash != &hash
        {
            return Err(StoreError::EventIdConflict);
        }
        return Ok((
            Receipt {
                message_id: event.message_id,
                store_seq: *seq,
                duplicate: true,
            },
            body,
        ));
    }
    let foreign: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM ingested_events WHERE producer_run_id=?1 AND session_id!=?2)",
        params![
            event.producer.run_id.to_string(),
            event.session_id.to_string()
        ],
        |r| r.get(0),
    )?;
    if foreign {
        return Err(StoreError::ScopeMismatch);
    }
    let sealed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM producer_seals WHERE producer_run_id=?1) OR EXISTS(SELECT 1 FROM producer_reconciliations WHERE producer_run_id=?1)",[event.producer.run_id.to_string()],|r|r.get(0))?;
    if sealed {
        return Err(StoreError::LateResult);
    }
    tx.execute("INSERT INTO ingested_events(message_id,event_id,session_id,producer_run_id,producer_seq,event_type,body_sha256,body_json)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8)", params![event.message_id.to_string(), event.event_id.to_string(), event.session_id.to_string(),
            event.producer.run_id.to_string(), event.producer.seq, event.event_type.as_str(), hash, body])?;
    Ok((
        Receipt {
            message_id: event.message_id,
            store_seq: tx.last_insert_rowid() as u64,
            duplicate: false,
        },
        body,
    ))
}

fn append_outbox<T>(
    tx: &Transaction<'_>,
    event: &Event<T>,
    receipt: &Receipt,
    body: &str,
) -> Result<()> {
    tx.execute("INSERT INTO outbox(store_seq,message_id,session_id,event_type,body_json) VALUES(?1,?2,?3,?4,?5)",
        params![receipt.store_seq, event.message_id.to_string(), event.session_id.to_string(), event.event_type.as_str(), body])?;
    Ok(())
}

fn capture_context(
    connection: &Connection,
    session_id: Uuid,
    segment_id: Uuid,
) -> Result<(Uuid, AudioRange, Option<u32>)> {
    let row: Option<(String, String, String, Option<u32>)> = connection
        .query_row(
            "SELECT s.session_id,s.track_id,c.audio_json,s.current_revision
        FROM segments s JOIN capture_segments c ON c.segment_id=s.id WHERE s.id=?1",
            [segment_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let (owner, track, audio, revision) = row.ok_or(StoreError::DependencyNotReady)?;
    if owner != session_id.to_string() {
        return Err(StoreError::ScopeMismatch);
    }
    Ok((parse_uuid(&track)?, serde_json::from_str(&audio)?, revision))
}

fn load_revision(
    connection: &Connection,
    session_id: Uuid,
    segment_id: Uuid,
    revision: Revision,
) -> Result<TranscriptRecord> {
    capture_context(connection, session_id, segment_id)?;
    let body: Option<String> = connection
        .query_row(
            "SELECT record_json FROM segment_revisions WHERE segment_id=?1 AND revision=?2",
            params![segment_id.to_string(), revision.get()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(serde_json::from_str(
        &body.ok_or(StoreError::DependencyNotReady)?,
    )?)
}

fn insert_revision(tx: &Transaction<'_>, record: &TranscriptRecord) -> Result<()> {
    let p = &record.payload;
    let status = match p.status {
        TranscriptStatus::Success => "success",
        TranscriptStatus::Empty => "empty",
        TranscriptStatus::Failed => "failed",
    };
    tx.execute("INSERT INTO segment_revisions(segment_id,revision,text,status,record_json,created_seq) VALUES(?1,?2,?3,?4,?5,?6)",
        params![p.segment_id.to_string(), p.revision.get(), p.text, status, serde_json::to_string(record)?, record.created_seq])?;
    tx.execute(
        "UPDATE segments SET current_revision=?1 WHERE id=?2",
        params![p.revision.get(), p.segment_id.to_string()],
    )?;
    Ok(())
}

fn insert_coverage(tx: &Transaction<'_>, record: &TranscriptRecord) -> Result<()> {
    let p = &record.payload;
    if p.status == TranscriptStatus::Success {
        let (epoch, targets_json): (u64, Option<String>) = tx.query_row(
            "SELECT direction_epoch,targets_json FROM capture_segments WHERE segment_id=?1",
            [p.segment_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let targets: Vec<String> = targets_json
            .map(|body| serde_json::from_str(&body))
            .transpose()?
            .unwrap_or_else(|| p.target_languages.clone());
        for target in &targets {
            tx.execute("INSERT INTO translation_coverage(segment_id,segment_revision,target_language,direction_epoch,start_utf8,end_utf8,state)
                VALUES(?1,?2,?3,?4,0,?5,'pending')", params![p.segment_id.to_string(), p.revision.get(), target, epoch, p.text.len()])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
