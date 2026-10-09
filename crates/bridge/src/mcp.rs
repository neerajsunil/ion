//! The MCP side: JSON-RPC 2.0 messages, the handshake methods the server
//! answers itself, and the tools Ion offers to the agent.

use serde_json::{Value, json};

/// The MCP version Ion speaks when the client doesn't name one.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// What to do with one incoming JSON-RPC message.
#[derive(Debug, PartialEq)]
pub(crate) enum Dispatch {
    /// Send this reply now.
    Reply(Value),
    /// A tool call for the IDE to answer: (request id, tool, arguments).
    Call(Value, String, Value),
    /// A notification or a reply; nothing to send.
    Ignore,
}

pub(crate) fn dispatch(message: &Value) -> Dispatch {
    let method = message.get("method").and_then(Value::as_str);
    let id = message.get("id").cloned();
    let (Some(method), Some(id)) = (method, id) else {
        return Dispatch::Ignore;
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let version = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOL_VERSION);
            json!({
                "protocolVersion": version,
                "capabilities": {
                    "logging": {},
                    "prompts": { "listChanged": true },
                    "resources": { "subscribe": false, "listChanged": false },
                    "tools": { "listChanged": true },
                },
                "serverInfo": { "name": "ion", "version": env!("CARGO_PKG_VERSION") },
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "prompts/list" => json!({ "prompts": [] }),
        "resources/list" => json!({ "resources": [] }),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            return Dispatch::Call(id, name.to_owned(), arguments);
        }
        _ => return Dispatch::Reply(error(id, -32601, &format!("Unknown method: {method}"))),
    };
    Dispatch::Reply(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

pub(crate) fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A tool result made of text blocks.
pub(crate) fn text_result(id: Value, texts: &[&str]) -> Value {
    let content: Vec<Value> = texts
        .iter()
        .map(|text| json!({ "type": "text", "text": text }))
        .collect();
    json!({ "jsonrpc": "2.0", "id": id, "result": { "content": content } })
}

pub(crate) fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
            "$schema": "http://json-schema.org/draft-07/schema#",
        },
    })
}

fn string(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn boolean(description: &str, default: bool) -> Value {
    json!({ "type": "boolean", "description": description, "default": default })
}

/// The tools Ion implements, named as Claude Code's IDE integrations name them.
fn tools() -> Vec<Value> {
    vec![
        tool(
            "openFile",
            "Open a file in the editor and optionally select a range of text",
            json!({
                "filePath": string("Path to the file to open"),
                "preview": boolean("Whether to open the file in preview mode", false),
                "startText": string("Text pattern to find the start of the selection range"),
                "endText": string("Text pattern to find the end of the selection range"),
                "selectToEndOfLine": boolean("Extend the selection to the end of the line", false),
                "makeFrontmost": boolean("Make the file the active editor tab", true),
            }),
            &["filePath"],
        ),
        tool(
            "openDiff",
            "Open a diff view comparing a file with new contents, and wait for the user to accept or reject it",
            json!({
                "old_file_path": string("Path to the file to compare"),
                "new_file_path": string("Path to the file the new contents are for"),
                "new_file_contents": string("The proposed contents of the file"),
                "tab_name": string("Name of the diff tab"),
            }),
            &[
                "old_file_path",
                "new_file_path",
                "new_file_contents",
                "tab_name",
            ],
        ),
        tool(
            "getCurrentSelection",
            "Get the current text selection in the active editor",
            json!({}),
            &[],
        ),
        tool(
            "getLatestSelection",
            "Get the most recent text selection, even if the editor is no longer active",
            json!({}),
            &[],
        ),
        tool(
            "getOpenEditors",
            "Get information about the files open in editor tabs",
            json!({}),
            &[],
        ),
        tool(
            "getWorkspaceFolders",
            "Get the folders open in the IDE",
            json!({}),
            &[],
        ),
        tool(
            "getDiagnostics",
            "Get errors and warnings for a file, or for all files when no uri is given",
            json!({ "uri": string("Optional file URI to get diagnostics for") }),
            &[],
        ),
        tool(
            "checkDocumentDirty",
            "Check whether a document has unsaved changes",
            json!({ "filePath": string("Path to the file to check") }),
            &["filePath"],
        ),
        tool(
            "saveDocument",
            "Save a document with unsaved changes",
            json!({ "filePath": string("Path to the file to save") }),
            &["filePath"],
        ),
        tool(
            "close_tab",
            "Close a tab by its name",
            json!({ "tab_name": string("Name of the tab to close") }),
            &["tab_name"],
        ),
        tool(
            "closeAllDiffTabs",
            "Close all diff tabs opened for the agent",
            json!({}),
            &[],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_the_handshake_and_forwards_tool_calls() {
        let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18" } });
        let Dispatch::Reply(reply) = dispatch(&init) else {
            panic!("initialize gets a reply");
        };
        assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(reply["id"], 1);

        let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
        let Dispatch::Reply(reply) = dispatch(&list) else {
            panic!("tools/list gets a reply");
        };
        let names: Vec<&str> = reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"openDiff") && names.contains(&"close_tab"));

        let call = json!({ "jsonrpc": "2.0", "id": "a", "method": "tools/call",
            "params": { "name": "openFile", "arguments": { "filePath": "x" } } });
        assert_eq!(
            dispatch(&call),
            Dispatch::Call(json!("a"), "openFile".into(), json!({ "filePath": "x" }))
        );

        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert_eq!(dispatch(&note), Dispatch::Ignore);
        let unknown = json!({ "jsonrpc": "2.0", "id": 3, "method": "nope" });
        let Dispatch::Reply(reply) = dispatch(&unknown) else {
            panic!("unknown methods get an error");
        };
        assert_eq!(reply["error"]["code"], -32601);
    }
}
