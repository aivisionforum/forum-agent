//! F09/F10: private session-scoped speaker labels and strictly public peer data.
use super::*;
macro_rules! dto {($($item:item)*)=>{$(#[derive(Debug,Clone,PartialEq,Eq,Serialize,Deserialize,JsonSchema)] #[serde(deny_unknown_fields)] $item)*};}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SpeakerLabel {
    Unknown,
    Overlap,
    Anonymous { speaker_id: Uuid },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SpeakerAssignmentOrigin {
    Automatic {
        model_manifest_id: String,
        model_version: String,
    },
    Human {
        operator_id: String,
        reason: String,
    },
}
dto! {
 pub struct PublicSelection {pub owner_device_id:Uuid,pub session_id:Uuid,pub public_id:Uuid,pub revision:Revision}
 pub struct PeerAnalysisProvenance {pub snapshot_id:Uuid,pub owner_device_id:Uuid,pub event_id:Uuid,pub session_id:Uuid,pub source_cursor:u64,pub public_id:Uuid,pub revision:Revision,pub artifact_alias_id:Uuid}
 pub struct SpeakerAssignmentCommand {pub request_id:Uuid,pub session_id:Uuid,pub segment_id:Uuid,pub source_revision:Revision,pub expected_revision:Option<Revision>,pub label:SpeakerLabel,pub origin:SpeakerAssignmentOrigin}
 pub struct SpeakerAssignment {pub session_id:Uuid,pub segment_id:Uuid,pub source_revision:Revision,pub revision:Revision,pub label:SpeakerLabel,pub origin:SpeakerAssignmentOrigin,pub created_at_ms:u64,pub current:bool}
 pub struct PeerSession {pub owner_device_id:Uuid,pub event_id:Uuid,pub session_id:Uuid,pub title:String,pub room_name:String}
 pub struct PeerSessionState {pub session:PeerSession,pub cursor:u64,pub last_sync_at_ms:Option<u64>,pub stale:bool}
 pub struct PeerPublicationSnapshot {pub owner_device_id:Uuid,pub event_id:Uuid,pub session_id:Uuid,pub cursor:u64,pub artifacts:Vec<PublicArtifact>}
 pub struct PeerPublicationBatch {pub owner_device_id:Uuid,pub event_id:Uuid,pub session_id:Uuid,pub after_cursor:u64,pub cursor:u64,pub changes:Vec<PublicationChange>}
 pub struct PublicSearchHit {pub owner_device_id:Uuid,pub session_id:Uuid,pub public_id:Uuid,pub revision:Revision,pub title:String,pub text:String,pub match_start_utf8:usize,pub match_end_utf8:usize,pub stale:bool,pub last_sync_at_ms:Option<u64>}
 pub struct PublicSessionView {pub owner_device_id:Uuid,pub event_id:Uuid,pub session_id:Uuid,pub title:String,pub room_name:String,pub local:bool,pub stale:bool,pub last_sync_at_ms:Option<u64>,pub cursor:u64,pub artifacts:Vec<PublicArtifact>}
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpeakerEmbeddingCommand {
    pub request_id: Uuid,
    pub session_id: Uuid,
    pub segment_id: Uuid,
    pub source_revision: Revision,
    pub expected_revision: Option<Revision>,
    pub model_manifest_id: String,
    pub model_version: String,
    pub pcm_sha256: String,
    pub embedding: Vec<f32>,
}
