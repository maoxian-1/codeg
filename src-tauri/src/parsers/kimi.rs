use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::models::*;
use crate::parsers::{folder_name_from_path, truncate_str, AgentParser, ParseError};

pub struct KimiParser {
    base_dir: PathBuf,
}

fn read_wire_timestamps(wire_path: &PathBuf) -> Vec<DateTime<Utc>> {
    let Ok(wire_content) = fs::read_to_string(wire_path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in wire_content.lines() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(ts) = value.get("timestamp").and_then(|t| t.as_f64()) {
                if let Some(dt) = DateTime::from_timestamp(ts as i64, ((ts % 1.0) * 1e9) as u32) {
                    out.push(dt);
                }
            }
        }
    }
    out
}

fn read_custom_title(session_dir: &PathBuf) -> Option<String> {
    let state_path = session_dir.join("state.json");
    let state_content = fs::read_to_string(state_path).ok()?;
    let state: serde_json::Value = serde_json::from_str(&state_content).ok()?;
    state
        .get("custom_title")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
}

fn is_archived(session_dir: &PathBuf) -> bool {
    let state_path = session_dir.join("state.json");
    let Ok(state_content) = fs::read_to_string(state_path) else {
        return false;
    };
    let Ok(state) = serde_json::from_str::<serde_json::Value>(&state_content) else {
        return false;
    };
    state
        .get("archived")
        .and_then(|a| a.as_bool())
        .unwrap_or(false)
}

fn flush_turn(
    role: TurnRole,
    blocks: &mut Vec<ContentBlock>,
    turns: &mut Vec<MessageTurn>,
    turn_index: &mut u32,
    timestamps: &[DateTime<Utc>],
) {
    if blocks.is_empty() {
        return;
    }
    turns.push(MessageTurn {
        id: format!("turn-{}", turn_index),
        role,
        blocks: std::mem::take(blocks),
        timestamp: timestamps
            .get(*turn_index as usize)
            .copied()
            .unwrap_or_else(Utc::now),
        usage: None,
        duration_ms: None,
        model: None,
        completed_at: None,
    });
    *turn_index += 1;
}

impl KimiParser {
    pub fn new() -> Self {
        let base_dir = resolve_kimi_home_dir().join("sessions");
        Self { base_dir }
    }

    fn parse_session_summary(
        &self,
        session_dir: &PathBuf,
        _project_hash: &str,
    ) -> Result<Option<ConversationSummary>, ParseError> {
        let state_path = session_dir.join("state.json");
        let context_path = session_dir.join("context.jsonl");

        if !state_path.exists() || !context_path.exists() {
            return Ok(None);
        }

        if is_archived(session_dir) {
            return Ok(None);
        }

        let custom_title = read_custom_title(session_dir);

        let wire_path = session_dir.join("wire.jsonl");
        let wire_timestamps = if wire_path.exists() {
            read_wire_timestamps(&wire_path)
        } else {
            Vec::new()
        };
        let first_timestamp = wire_timestamps.first().copied();
        let last_timestamp = wire_timestamps.last().copied();

        let file = fs::File::open(&context_path)?;
        let reader = BufReader::new(file);

        let mut title = custom_title;
        let mut message_count: u32 = 0;
        let mut folder_path: Option<String> = None;

        let session_id = session_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };
            if line.trim().is_empty() {
                continue;
            }

            let value: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let role = value.get("role").and_then(|r| r.as_str()).unwrap_or("");

            match role {
                "user" => {
                    message_count += 1;
                    if title.is_none() {
                        if let Some(content) = value.get("content") {
                            if let Some(text) = content.as_str() {
                                let trimmed = text.trim();
                                if !trimmed.is_empty() {
                                    title = Some(truncate_str(trimmed, 100));
                                }
                            }
                        }
                    }
                }
                "_system_prompt" => {
                    if folder_path.is_none() {
                        if let Some(content) = value.get("content").and_then(|c| c.as_str()) {
                            if let Some(cwd) = extract_cwd_from_system_prompt(content) {
                                folder_path = Some(cwd);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        let title = title.unwrap_or_else(|| session_id.clone());

        let started_at = match first_timestamp {
            Some(ts) => ts,
            None => {
                let metadata = fs::metadata(session_dir).ok();
                let created = metadata
                    .and_then(|m| m.created().ok())
                    .map(|t| {
                        let duration = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
                        DateTime::from_timestamp(duration.as_secs() as i64, 0)
                    })
                    .flatten();
                match created {
                    Some(ts) => ts,
                    None => return Ok(None),
                }
            }
        };

        let folder_name = folder_path.as_ref().map(|p| folder_name_from_path(p));

        Ok(Some(ConversationSummary {
            id: session_id,
            agent_type: AgentType::KimiCli,
            folder_path,
            folder_name,
            title: Some(title),
            started_at,
            ended_at: last_timestamp,
            message_count,
            model: None,
            git_branch: None,
        }))
    }

    fn parse_conversation_detail(
        &self,
        session_dir: &PathBuf,
        conversation_id: &str,
    ) -> Result<ConversationDetail, ParseError> {
        let context_path = session_dir.join("context.jsonl");

        if !context_path.exists() {
            return Err(ParseError::ConversationNotFound(
                conversation_id.to_string(),
            ));
        }

        let title = read_custom_title(session_dir);

        let wire_path = session_dir.join("wire.jsonl");
        let timestamps = if wire_path.exists() {
            read_wire_timestamps(&wire_path)
        } else {
            Vec::new()
        };

        let file = fs::File::open(&context_path)?;
        let reader = BufReader::new(file);

        let mut turns: Vec<MessageTurn> = Vec::new();
        let mut current_turn_blocks: Vec<ContentBlock> = Vec::new();
        let mut current_role: Option<TurnRole> = None;
        let mut turn_index = 0u32;
        let mut folder_path: Option<String> = None;

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };
            if line.trim().is_empty() {
                continue;
            }

            let value: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let role = value.get("role").and_then(|r| r.as_str()).unwrap_or("");

            match role {
                "user" => {
                    if let Some(prev_role) = current_role.take() {
                        flush_turn(
                            prev_role,
                            &mut current_turn_blocks,
                            &mut turns,
                            &mut turn_index,
                            &timestamps,
                        );
                    }

                    current_role = Some(TurnRole::User);
                    current_turn_blocks.extend(extract_content_blocks(&value));
                }
                "assistant" => {
                    match current_role.as_ref() {
                        Some(TurnRole::User) => {
                            if let Some(prev_role) = current_role.take() {
                                flush_turn(
                                    prev_role,
                                    &mut current_turn_blocks,
                                    &mut turns,
                                    &mut turn_index,
                                    &timestamps,
                                );
                            }
                            current_role = Some(TurnRole::Assistant);
                        }
                        Some(TurnRole::Assistant) => {
                            // Keep current role to merge consecutive assistant messages
                        }
                        _ => {
                            current_role = Some(TurnRole::Assistant);
                        }
                    }

                    current_turn_blocks.extend(extract_content_blocks(&value));
                }
                "_system_prompt" => {
                    if folder_path.is_none() {
                        if let Some(content) = value.get("content").and_then(|c| c.as_str()) {
                            if let Some(cwd) = extract_cwd_from_system_prompt(content) {
                                folder_path = Some(cwd);
                            }
                        }
                    }
                }
                "_checkpoint" => continue,
                _ => {}
            }
        }

        if let Some(role) = current_role.take() {
            flush_turn(
                role,
                &mut current_turn_blocks,
                &mut turns,
                &mut turn_index,
                &timestamps,
            );
        }

        let started_at = timestamps.first().copied().unwrap_or_else(Utc::now);
        let ended_at = timestamps.last().copied();
        let message_count = turns.len() as u32;

        let folder_name = folder_path.as_ref().map(|p| folder_name_from_path(p));
        let summary = ConversationSummary {
            id: conversation_id.to_string(),
            agent_type: AgentType::KimiCli,
            folder_path,
            folder_name,
            title,
            started_at,
            ended_at,
            message_count,
            model: None,
            git_branch: None,
        };

        Ok(ConversationDetail {
            summary,
            turns,
            session_stats: None,
        })
    }
}

fn extract_content_blocks(value: &serde_json::Value) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    let content = match value.get("content") {
        Some(c) => c,
        None => return blocks,
    };

    if let Some(text) = content.as_str() {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            blocks.push(ContentBlock::Text {
                text: trimmed.to_string(),
            });
        }
        return blocks;
    }

    if let Some(arr) = content.as_array() {
        for item in arr {
            let block_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match block_type {
                "text" => {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            blocks.push(ContentBlock::Text {
                                text: trimmed.to_string(),
                            });
                        }
                    }
                }
                "thinking" => {
                    if let Some(text) = item.get("thinking").and_then(|t| t.as_str()) {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            blocks.push(ContentBlock::Thinking {
                                text: trimmed.to_string(),
                            });
                        }
                    }
                }
                "tool_use" => {
                    let tool_use_id = item
                        .get("id")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string());
                    let tool_name = item
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let input_preview = item.get("input").map(|i| i.to_string());
                    blocks.push(ContentBlock::ToolUse {
                        tool_use_id,
                        tool_name,
                        input_preview,
                    });
                }
                _ => {}
            }
        }
    }

    blocks
}

fn extract_cwd_from_system_prompt(content: &str) -> Option<String> {
    let patterns = [
        "The current working directory is `",
        "current working directory is `",
    ];

    for pattern in &patterns {
        if let Some(start) = content.find(pattern) {
            let after_pattern = &content[start + pattern.len()..];
            if let Some(end) = after_pattern.find('`') {
                let cwd = &after_pattern[..end];
                if !cwd.is_empty() {
                    return Some(cwd.to_string());
                }
            }
        }
    }

    None
}

fn resolve_kimi_home_dir() -> PathBuf {
    resolve_kimi_home_dir_from(std::env::var_os("KIMI_HOME"), dirs::home_dir())
}

fn resolve_kimi_home_dir_from(
    kimi_home_env: Option<std::ffi::OsString>,
    home_dir: Option<PathBuf>,
) -> PathBuf {
    kimi_home_env
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir.unwrap_or_default().join(".kimi"))
}

impl AgentParser for KimiParser {
    fn list_conversations(&self) -> Result<Vec<ConversationSummary>, ParseError> {
        let mut conversations = Vec::new();

        if !self.base_dir.exists() {
            return Ok(conversations);
        }

        for project_entry in fs::read_dir(&self.base_dir)? {
            let project_entry = match project_entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let project_dir = project_entry.path();
            if !project_dir.is_dir() {
                continue;
            }

            let project_hash = project_dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            for session_entry in fs::read_dir(&project_dir)? {
                let session_entry = match session_entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                let session_dir = session_entry.path();
                if !session_dir.is_dir() {
                    continue;
                }

                match self.parse_session_summary(&session_dir, &project_hash) {
                    Ok(Some(summary)) => {
                        conversations.push(summary);
                    }
                    Ok(None) => continue,
                    Err(_) => continue,
                }
            }
        }

        conversations.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(conversations)
    }

    fn get_conversation(&self, conversation_id: &str) -> Result<ConversationDetail, ParseError> {
        if !self.base_dir.exists() {
            return Err(ParseError::ConversationNotFound(
                conversation_id.to_string(),
            ));
        }

        for project_entry in fs::read_dir(&self.base_dir)? {
            let project_entry = match project_entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let project_dir = project_entry.path();
            if !project_dir.is_dir() {
                continue;
            }

            let session_dir = project_dir.join(conversation_id);
            if session_dir.exists() && session_dir.is_dir() {
                return self.parse_conversation_detail(&session_dir, conversation_id);
            }
        }

        Err(ParseError::ConversationNotFound(
            conversation_id.to_string(),
        ))
    }
}
