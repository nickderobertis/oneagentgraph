//! Resolve complete candidate content from a durable transcript reference.

use serde_json::Value;

#[derive(Clone, Copy)]
pub(crate) enum Content {
    Instruction,
    Text,
    Input,
    Output,
}

/// Resolve the published candidate and event, preserving the complete content.
pub(crate) fn resolve(reference: &Value, field: Content) -> Result<String, (String, String)> {
    use std::io::BufRead;
    let invalid = |error: serde_json::Error| ("<invalid reference>".to_string(), error.to_string());
    let (reference, activity_index) = if matches!(field, Content::Input | Content::Output) {
        if reference.get("index").is_none() {
            return Err((
                "<invalid reference>".to_string(),
                "activity reference has no index".to_string(),
            ));
        }
        let activity: crate::event::ActivityTranscript =
            serde_json::from_value(reference.clone()).map_err(invalid)?;
        (activity.location, Some(activity.index))
    } else {
        let location: crate::event::Transcript =
            serde_json::from_value(reference.clone()).map_err(invalid)?;
        (location, None)
    };
    let path = std::path::Path::new(&reference.history_dir)
        .join(&reference.history_project)
        .join(format!("{}.jsonl", reference.history_session));
    let unavailable = |why: String| (path.display().to_string(), why);
    // The history producer validates these components too; journals are external
    // input, so a reader must check before joining their address on its host.
    use oneharness_core::domain::history::{AbsoluteHistoryDir, HistoryPathComponent};
    if AbsoluteHistoryDir::try_from(std::path::PathBuf::from(&reference.history_dir)).is_err()
        || HistoryPathComponent::try_from(reference.history_project.clone()).is_err()
        || HistoryPathComponent::try_from(reference.history_session.clone()).is_err()
    {
        return Err(unavailable("invalid history address".to_string()));
    }
    let file = std::fs::File::open(&path).map_err(|error| unavailable(error.to_string()))?;
    for line in std::io::BufReader::new(file).lines() {
        let line = line.map_err(|error| unavailable(error.to_string()))?;
        let line: Value =
            serde_json::from_str(&line).map_err(|error| unavailable(error.to_string()))?;
        let candidate = if matches!(field, Content::Input | Content::Output) {
            &line["run_id"]
        } else {
            &line["history_id"]
        };
        if candidate.as_str() != Some(&reference.history_id) {
            continue;
        }
        match field {
            Content::Instruction if line["type"] == "run-started" => {
                return line["prompt"]
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| unavailable("run-started prompt is not a string".to_string()));
            }
            Content::Text if line["type"] == "run" => {
                return line["text"]
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| unavailable("run text is not a string".to_string()));
            }
            Content::Input | Content::Output if line["type"] == "event" => {
                let index = line["event"]["index"]
                    .as_u64()
                    .ok_or_else(|| unavailable("event index is not an integer".to_string()))?;
                if Some(index) != activity_index {
                    continue;
                }
                let value = &line["event"][if matches!(field, Content::Input) {
                    "input"
                } else {
                    "output"
                }];
                return match value {
                    Value::String(value) => Ok(value.clone()),
                    Value::Null => Ok(String::new()),
                    other if matches!(field, Content::Input) => Ok(other.to_string()),
                    _ => Err(unavailable("event output is not a string".to_string())),
                };
            }
            _ => (),
        }
    }
    Err(unavailable(format!(
        "entry missing for {}{}",
        reference.history_id,
        activity_index.map_or(String::new(), |index| format!(" event {index}"))
    )))
}
