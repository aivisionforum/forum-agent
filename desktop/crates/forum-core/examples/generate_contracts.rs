//! Emit JSON Schema and TypeScript from the exact Rust producer/consumer types.
//! Run with --check in CI. This program performs no network or model work.
use forum_contracts::*;
use forum_core::*;
use schemars::{schema_for, JsonSchema};
use serde_json::{json, Map, Value};
use std::{collections::BTreeSet, path::PathBuf};

fn add<T: JsonSchema>(defs: &mut Map<String, Value>, name: &str) {
    let mut schema = serde_json::to_value(schema_for!(T)).unwrap();
    fn strip_titles(v: &mut Value) {
        match v {
            Value::Object(m) => {
                if m.get("title").is_some_and(Value::is_string) {
                    m.remove("title");
                }
                for v in m.values_mut() {
                    strip_titles(v);
                }
            }
            Value::Array(a) => {
                for v in a {
                    strip_titles(v)
                }
            }
            _ => (),
        }
    }
    strip_titles(&mut schema);
    if let Some(Value::Object(children)) = schema.as_object_mut().unwrap().remove("$defs") {
        for (key, value) in children {
            if let Some(old) = defs.insert(key.clone(), value.clone()) {
                assert_eq!(old, value, "conflicting {key}");
            }
        }
    }
    schema.as_object_mut().unwrap().remove("$schema");
    schema.as_object_mut().unwrap().remove("title");
    // A named root may already exist as a nested definition, including its title.
    if let Some(old) = defs.get_mut(name) {
        old.as_object_mut().unwrap().remove("title");
        assert_eq!(*old, schema, "conflicting {name}");
    }
    defs.insert(name.into(), schema);
}
fn add_event<T: JsonSchema>(defs: &mut Map<String, Value>, name: &str, kind: EventType) {
    add::<Event<T>>(defs, name);
    let schema = defs.get_mut(name).unwrap();
    schema["properties"]["type"] = json!({"type":"string","const":kind.as_str()});
    schema["properties"]["schema_version"] = json!({"type":"integer","const":SCHEMA_VERSION});
}
fn bounds(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if map
                .get("maximum")
                .and_then(Value::as_f64)
                .is_some_and(|n| n > 9_007_199_254_740_991.0)
            {
                map.insert("maximum".into(), json!(9_007_199_254_740_991u64));
            }
            if map.get("format").and_then(Value::as_str) == Some("uuid") {
                map.insert(
                    "not".into(),
                    json!({"const":"00000000-0000-0000-0000-000000000000"}),
                );
            }
            if let Some(Value::Object(props)) = map.get_mut("properties") {
                for name in ["attempt", "direction_epoch", "seq", "sample_rate"] {
                    if let Some(p) = props.get_mut(name).and_then(Value::as_object_mut) {
                        p.insert("minimum".into(), json!(1));
                    }
                }
                if let Some(p) = props.get_mut("source_spans").and_then(Value::as_object_mut) {
                    p.insert("minItems".into(), json!(1));
                }
            }
            for child in map.values_mut() {
                bounds(child);
            }
        }
        Value::Array(values) => {
            for child in values {
                bounds(child)
            }
        }
        _ => (),
    }
}
fn ts(schema: &Value) -> String {
    if schema == &Value::Bool(true) {
        return "unknown".into();
    }
    if schema == &Value::Bool(false) {
        return "never".into();
    }
    if let Some(reference) = schema["$ref"].as_str() {
        return reference
            .strip_prefix("#/$defs/")
            .expect("local definition")
            .into();
    }
    if let Some(value) = schema.get("const") {
        return value.to_string();
    }
    if let Some(values) = schema["enum"].as_array() {
        return values
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join(" | ");
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(values) = schema[key].as_array() {
            return values.iter().map(ts).collect::<Vec<_>>().join(" | ");
        }
    }
    if let Some(values) = schema["type"].as_array() {
        return values
            .iter()
            .map(|v| {
                let mut s = schema.clone();
                s["type"] = v.clone();
                ts(&s)
            })
            .collect::<Vec<_>>()
            .join(" | ");
    }
    match schema["type"].as_str() {
        Some("string") => "string".into(),
        Some("integer" | "number") => "number".into(),
        Some("boolean") => "boolean".into(),
        Some("null") => "null".into(),
        Some("array") => format!("Array<{}>", ts(&schema["items"])),
        Some("object") => {
            let required: BTreeSet<_> = schema["required"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let mut fields = Vec::new();
            if let Some(properties) = schema["properties"].as_object() {
                for (key, value) in properties {
                    fields.push(format!(
                        "  {}{}: {};",
                        serde_json::to_string(key).unwrap(),
                        if required.contains(key.as_str()) {
                            ""
                        } else {
                            "?"
                        },
                        ts(value)
                    ));
                }
            }
            if let Some(Value::Object(extra)) = schema.get("additionalProperties") {
                fields.push(format!(
                    "  [key: string]: {};",
                    ts(&Value::Object(extra.clone()))
                ));
            }
            format!("{{\n{}\n}}", fields.join("\n"))
        }
        None if schema.as_object().is_some_and(|o| {
            o.keys()
                .all(|k| matches!(k.as_str(), "description" | "title" | "default" | "examples"))
        }) =>
        {
            "unknown".into()
        }
        unsupported => panic!("unsupported schema node {unsupported:?}: {schema}"),
    }
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("packages/contracts");
    let check = std::env::args().any(|a| a == "--check");
    let mut defs = Map::new();
    macro_rules! ty {($($t:ty),*$(,)?)=>{$(add::<$t>(&mut defs,stringify!($t));)*};}
    ty!(
        SessionSpec,
        TrackSpec,
        AudioRange,
        Producer,
        Revision,
        SourceSpan,
        SessionState,
        SessionTransition,
        AudioGap,
        TrackSeal,
        CaptureStopped,
        ProducerSeal,
        ProducerRecoveryEvidence,
        ProducerReconciled,
        TranscriptSeal,
        CaptureSegmentClosed,
        TranscriptFinal,
        TranscriptRevision,
        DirectionChanged,
        DirectionBoundary,
        TranslationRequested,
        TranslationFinal,
        TranslationFailed,
        Receipt,
        TranscriptRecord,
        RevisionOrigin,
        IncompleteSegment,
        SessionSnapshot,
        CoverageIntent,
        OutboxMessage,
        SessionStatus,
        SessionSummary,
        RecoverySegment,
        RecoveryRevision,
        PageKey,
        SnapshotItem,
        SnapshotPage,
        TranslationWork,
        TranslationRecord,
        TranslationPageKey,
        TranslationPage,
        SessionExport
    );
    let mut events = Vec::new();
    macro_rules! event {($payload:ty,$variant:ident)=>{{let name=concat!(stringify!($payload),"Event");add_event::<$payload>(&mut defs,name,EventType::$variant);events.push(json!({"$ref":format!("#/$defs/{name}")}));}};}
    event!(CaptureSegmentClosed, AudioSegmentClosed);
    event!(TranscriptFinal, TranscriptFinal);
    event!(TranscriptRevision, TranscriptRevised);
    event!(SessionTransition, SessionChanged);
    event!(AudioGap, AudioGap);
    event!(CaptureStopped, CaptureStopped);
    event!(ProducerSeal, ProducerSealed);
    event!(ProducerReconciled, ProducerReconciled);
    event!(TranscriptSeal, TranscriptSealed);
    event!(DirectionChanged, DirectionChanged);
    event!(TranslationRequested, TranslationRequested);
    event!(TranslationFinal, TranslationFinal);
    event!(TranslationFailed, TranslationFailed);
    defs.insert("ForumEvent".into(), json!({"oneOf":events}));
    if let Some(revision) = defs.get_mut("Revision") {
        revision["minimum"] = json!(1);
    }
    let mut schema = json!({"$schema":"https://json-schema.org/draft/2020-12/schema","$id":"urn:ai-vision-forum:contracts:1","schema_version":SCHEMA_VERSION,"$ref":"#/$defs/ForumEvent","$defs":defs});
    bounds(&mut schema);
    assert!(schema["$defs"]["SessionSpec"]["properties"]["title"].is_object());
    for (name, definition) in schema["$defs"].as_object().unwrap() {
        if let Some(required) = definition["required"].as_array() {
            for key in required {
                assert!(
                    definition["properties"]
                        .get(key.as_str().unwrap())
                        .is_some(),
                    "required property lost in {name}: {key}"
                );
            }
        }
    }
    let schema_text = format!("{}\n", serde_json::to_string_pretty(&schema)?);
    let mut types=String::from("// GENERATED by forum-core/examples/generate_contracts.rs. Do not edit.\n// UTF-8 offsets are bytes, never JavaScript string indices. Validate wire data before use.\nexport const SCHEMA_VERSION = 1 as const;\n\n");
    for (name, value) in schema["$defs"].as_object().unwrap() {
        types.push_str(&format!("export type {name} = {};\n\n", ts(value)));
    }
    types.truncate(types.trim_end().len());
    types.push('\n');
    for (name, content) in [
        ("forum.schema.json", schema_text),
        ("forum.generated.ts", types),
    ] {
        let path = root.join(name);
        if check {
            if std::fs::read_to_string(&path)? != content {
                return Err(format!("generated contract is stale: {}", path.display()).into());
            }
        } else {
            std::fs::create_dir_all(&root)?;
            std::fs::write(path, content)?;
        }
    }
    Ok(())
}
