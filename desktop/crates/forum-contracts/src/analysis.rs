//! F05–F08 DTOs. Source truth and review/publication are independent state machines.
use super::*;

macro_rules! dto {($($item:item)*)=>{$(#[derive(Debug,Clone,PartialEq,Eq,Serialize,Deserialize,JsonSchema)] #[serde(deny_unknown_fields)] $item)*};}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisKind {
    Insight,
    Minutes,
    EventReport,
    SuggestedQuestions,
    RedactionReview,
    ClosingBrief,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisJobState {
    Queued,
    Waiting,
    Running,
    CancelRequested,
    Cancelled,
    Succeeded,
    SucceededPartial,
    Failed,
    Interrupted,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    Fact,
    Decision,
    Action,
    Risk,
    Question,
    Uncertainty,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroundingStatus {
    Cited,
    Unsupported,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    Processed,
    Failed,
    IgnoredEmpty,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactValidation {
    Valid,
    NeedsReview,
    Invalid,
    Stale,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactReview {
    Draft,
    Approved,
    Rejected,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactPublication {
    Private,
    Published,
    Hidden,
    Withdrawn,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AnalysisEvidence {
    Source {
        session_id: Uuid,
        span: SourceSpan,
    },
    Artifact {
        artifact_id: Uuid,
        revision: Revision,
        start_utf8: usize,
        end_utf8: usize,
        quote: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CoverageTarget {
    Source {
        segment_id: Uuid,
        segment_revision: Option<Revision>,
    },
    Artifact {
        artifact_id: Uuid,
        revision: Revision,
    },
}
dto! {
    pub struct AnalysisConfig {
        pub model_profile:String,pub model_manifest_id:String,pub prompt_version:String,pub prompt_sha256:String,
        pub profile_id:String,pub profile_version:String,pub profile_sha256:String,pub effective_config_hash:String,
        pub projection_policy_hash:String,pub generation:serde_json::Value,
    }
    pub struct CreateAnalysisJob {
        pub request_id:Uuid,pub session_ids:Vec<Uuid>,pub kind:AnalysisKind,
        pub config:AnalysisConfig,pub budget_ms:u64,pub max_attempts:u32,pub automatic:bool,
    }
    pub struct AnalysisSource {
        pub session_id:Uuid,pub segment_id:Uuid,pub revision:Option<Revision>,
        pub text:String,pub status:Option<TranscriptStatus>,pub audio:AudioRange,pub speaker_id:Option<Uuid>,
    }
    pub struct AnalysisPublishedInput {
        pub artifact_id:Uuid,pub revision:Revision,pub session_ids:Vec<Uuid>,pub kind:AnalysisKind,
        pub title:String,pub text:String,
    }
    pub struct AnalysisSnapshot {
        pub schema_version:u32,pub snapshot_id:Uuid,pub event_id:Uuid,pub session_ids:Vec<Uuid>,
        pub input_cursor:u64,pub kind:AnalysisKind,pub effective_config_hash:String,
        pub segments:Vec<AnalysisSource>,pub published_artifacts:Vec<AnalysisPublishedInput>,pub input_complete:bool,
    }
    pub struct AnalysisProgress {pub phase:String,pub completed_units:u32,pub total_units:u32,pub wait_reason:Option<String>}
    pub struct ArtifactRef {pub artifact_id:Uuid,pub revision:Revision}
    pub struct AnalysisJob {
        pub job_id:Uuid,pub request_id:Uuid,pub kind:AnalysisKind,pub event_id:Uuid,pub session_ids:Vec<Uuid>,
        pub state:AnalysisJobState,pub attempt:u32,pub max_attempts:u32,pub budget_ms:u64,
        pub created_at_ms:u64,pub deadline_at_ms:u64,pub started_at_ms:Option<u64>,
        pub snapshot_id:Uuid,pub snapshot_sha256:String,pub config:AnalysisConfig,
        pub automatic:bool,pub progress:AnalysisProgress,pub error:Option<String>,pub result:Option<ArtifactRef>,
    }
    pub struct AnalysisClaim {
        pub claim_id:Uuid,pub kind:ClaimKind,pub text:String,pub evidence:Vec<AnalysisEvidence>,
        pub grounding:GroundingStatus,pub assignee:Option<String>,pub due:Option<String>,
    }
    pub struct AnalysisTopic {pub label:String,pub evidence:Vec<AnalysisEvidence>}
    pub struct AnalysisSection {
        pub heading:String,pub claims:Vec<AnalysisClaim>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub topics:Vec<AnalysisTopic>,
    }
    pub struct ArtifactContent {pub title:String,pub sections:Vec<AnalysisSection>}
    pub struct CoverageUnit {
        pub target:CoverageTarget,pub start_utf8:usize,pub end_utf8:usize,
        pub status:CoverageStatus,pub reason:Option<String>,
    }
    pub struct AnalysisCoverage {pub units:Vec<CoverageUnit>}
    pub struct AnalysisResult {
        pub schema_version:u32,pub job_id:Uuid,pub attempt:u32,pub snapshot_id:Uuid,pub snapshot_sha256:String,
        pub effective_config_hash:String,pub content:ArtifactContent,pub coverage:AnalysisCoverage,
    }
    pub struct AnalysisCheckpoint {
        pub job_id:Uuid,pub attempt:u32,pub step_index:u32,pub snapshot_sha256:String,pub input_sha256:String,pub effective_config_hash:String,
        pub result_sha256:String,pub result:serde_json::Value,
    }
    pub struct ArtifactRecord {
        pub artifact_id:Uuid,pub revision:Revision,pub kind:AnalysisKind,pub event_id:Uuid,pub session_ids:Vec<Uuid>,
        pub job_id:Uuid,pub snapshot_id:Uuid,pub content:ArtifactContent,pub coverage:AnalysisCoverage,
        pub coverage_complete:bool,pub validation:ArtifactValidation,pub review:ArtifactReview,
        pub publication:ArtifactPublication,pub config:AnalysisConfig,pub created_at_ms:u64,
        pub operator_id:Option<String>,pub reason:Option<String>,
    }
    pub struct ArtifactEdit {
        pub artifact_id:Uuid,pub expected_revision:Revision,pub content:ArtifactContent,
        pub operator_id:String,pub reason:String,
    }
    pub struct ArtifactReviewCommand {
        pub artifact_id:Uuid,pub expected_revision:Revision,pub review:ArtifactReview,
        pub operator_id:String,pub reason:String,
    }
    pub struct PublicEvidenceInput {pub evidence:AnalysisEvidence,pub reviewed_text:String}
    pub struct ArtifactPublishCommand {
        pub artifact_id:Uuid,pub expected_revision:Revision,pub operator_id:String,pub reason:String,
        pub policy_hash:String,pub reviewed_title:String,pub reviewed_text:String,pub evidence:Vec<PublicEvidenceInput>,
    }
    pub struct ArtifactVisibilityCommand {
        pub artifact_id:Uuid,pub expected_revision:Revision,pub publication:ArtifactPublication,
        pub operator_id:String,pub reason:String,
    }
    pub struct PublicEvidence {pub public_evidence_id:Uuid,pub revision:Revision,pub text:String}
    pub struct PublicArtifact {
        pub public_id:Uuid,pub revision:Revision,pub kind:AnalysisKind,pub title:String,pub text:String,
        pub evidence:Vec<PublicEvidence>,pub publication_seq:u64,
    }
    pub struct PublicSnapshot {pub cursor:u64,pub artifacts:Vec<PublicArtifact>}
    pub struct PublicationChange {pub publication_seq:u64,pub public_id:Uuid,pub withdrawn:bool,pub artifact:Option<PublicArtifact>}
}
dto! {
    pub struct AnalysisSnapshotData {pub snapshot:AnalysisSnapshot,pub compact_json:String,pub sha256:String}
    pub struct AnalysisPageKey {pub cursor:u64,pub created_at_ms:u64,pub id:Uuid}
    pub struct AnalysisJobPage {pub cursor:u64,pub items:Vec<AnalysisJob>,pub next_after:Option<AnalysisPageKey>}
    pub struct ArtifactPage {pub cursor:u64,pub items:Vec<ArtifactRecord>,pub next_after:Option<AnalysisPageKey>}
    pub struct AnalysisEvidenceView {pub text:String,pub quote:String,pub current:bool}
}
impl AnalysisConfig {
    /// Hash all effective settings except the digest field itself.
    pub fn computed_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut value = serde_json::to_value(self).expect("serializable config");
        value
            .as_object_mut()
            .unwrap()
            .remove("effective_config_hash");
        format!("{:x}", Sha256::digest(canonical_json(&value).as_bytes()))
    }
}

/// Compact UTF-8 JSON with every object explicitly sorted by key. This must not
/// depend on serde_json's optional `preserve_order` feature being unified by a
/// desktop dependency. Arrays retain semantic order; number spelling is serde's.
pub fn canonical_json(value: &serde_json::Value) -> String {
    fn write(value: &serde_json::Value, out: &mut String) {
        match value {
            serde_json::Value::Object(map) => {
                out.push('{');
                let mut keys = map.keys().collect::<Vec<_>>();
                keys.sort();
                for (index, key) in keys.into_iter().enumerate() {
                    if index > 0 {
                        out.push(',')
                    }
                    out.push_str(&serde_json::to_string(key).expect("JSON key"));
                    out.push(':');
                    write(&map[key], out);
                }
                out.push('}');
            }
            serde_json::Value::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',')
                    }
                    write(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&serde_json::to_string(other).expect("JSON scalar")),
        }
    }
    let mut result = String::new();
    write(value, &mut result);
    result
}
#[cfg(test)]
mod canonical_tests {
    use super::*;
    #[test]
    fn canonical_objects_ignore_map_feature_and_insertion_order() {
        let value: serde_json::Value =
            serde_json::from_str(r#"{"z":[2,1],"a":{"c":0.0,"b":"📝"}}"#).unwrap();
        assert_eq!(
            canonical_json(&value),
            r#"{"a":{"b":"📝","c":0.0},"z":[2,1]}"#
        );
        let other: serde_json::Value =
            serde_json::from_str(r#"{"a":{"b":"📝","c":0.0},"z":[2,1]}"#).unwrap();
        assert_eq!(canonical_json(&value), canonical_json(&other));
    }
}
