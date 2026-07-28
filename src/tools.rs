//! MCP tool definitions (JSON Schemas advertised to the client).

use serde_json::{json, Value};

/// Build the tool list. When impersonation is not locked, each tool also
/// advertises an optional `on_behalf_of` argument.
pub fn tools(lock_on_behalf_of: bool) -> Value {
    let mut list = vec![
        json!({
            "name": "redmine_list_projects",
            "description": "List projects visible to the authenticated user.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "description": "Max results (default 25, max 100)", "minimum": 1, "maximum": 100 },
                    "offset": { "type": "integer", "description": "Pagination offset", "minimum": 0 }
                }
            }
        }),
        json!({
            "name": "redmine_get_project",
            "description": "Get a single project by id, identifier, or display name.",
            "inputSchema": {
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": { "type": "string", "description": "Numeric id, identifier, or display name (e.g. 'Bluehive AI')" }
                }
            }
        }),
        json!({
            "name": "redmine_list_issues",
            "description": "List/search issues with optional filters. project_id/status_id/tracker_id/priority_id accept ids or display names.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Project id, identifier, or display name" },
                    "assigned_to_id": { "type": "string", "description": "User id, 'me', login, email, full name, or group id" },
                    "author_id": { "type": "string", "description": "User id, login, email, or full name" },
                    "status_id": { "type": "string", "description": "'open', 'closed', '*', a status name, or a numeric id" },
                    "tracker_id": { "type": "string", "description": "Tracker id or name (e.g. 'Bug')" },
                    "priority_id": { "type": "string", "description": "Priority id or name" },
                    "subject": { "type": "string", "description": "Match against the subject (use '~term' for contains)" },
                    "query_id": { "type": "integer", "description": "Saved query id" },
                    "sort": { "type": "string", "description": "Sort field, e.g. 'updated_on:desc'" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
                    "offset": { "type": "integer", "minimum": 0 }
                }
            }
        }),
        json!({
            "name": "redmine_get_issue",
            "description": "Get a single issue with journals, attachments, children, and relations.",
            "inputSchema": {
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": { "type": "integer", "description": "Issue id" },
                    "include": { "type": "string", "description": "Comma-separated include list" }
                }
            }
        }),
        json!({
            "name": "redmine_create_issue",
            "description": "Create a new issue.",
            "inputSchema": {
                "type": "object",
                "required": ["project_id", "subject"],
                "properties": {
                    "project_id": { "type": "string", "description": "Project id, identifier, or display name" },
                    "subject": { "type": "string" },
                    "description": { "type": "string" },
                    "tracker_id": { "type": "string", "description": "Tracker id or name" },
                    "status_id": { "type": "string", "description": "Status id or name" },
                    "priority_id": { "type": "string", "description": "Priority id or name" },
                    "assigned_to_id": { "type": "string", "description": "User id, login, email, or full name" },
                    "category_id": { "type": "integer" },
                    "fixed_version_id": { "type": "integer" },
                    "parent_issue_id": { "type": "integer" },
                    "start_date": { "type": "string", "description": "YYYY-MM-DD" },
                    "due_date": { "type": "string", "description": "YYYY-MM-DD" },
                    "estimated_hours": { "type": "number" },
                    "done_ratio": { "type": "integer", "minimum": 0, "maximum": 100 }
                }
            }
        }),
        json!({
            "name": "redmine_update_issue",
            "description": "Update an existing issue. Any provided fields are updated.",
            "inputSchema": {
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": { "type": "integer" },
                    "subject": { "type": "string" },
                    "description": { "type": "string" },
                    "notes": { "type": "string", "description": "Add a journal note (comment)" },
                    "private_notes": { "type": "boolean" },
                    "status_id": { "type": "string", "description": "Status id or name" },
                    "priority_id": { "type": "string", "description": "Priority id or name" },
                    "assigned_to_id": { "type": "string", "description": "User id, login, email, or full name" },
                    "tracker_id": { "type": "string", "description": "Tracker id or name" },
                    "category_id": { "type": "integer" },
                    "fixed_version_id": { "type": "integer" },
                    "done_ratio": { "type": "integer", "minimum": 0, "maximum": 100 },
                    "due_date": { "type": "string" },
                    "start_date": { "type": "string" },
                    "estimated_hours": { "type": "number" }
                }
            }
        }),
        json!({
            "name": "redmine_add_issue_note",
            "description": "Add a comment (journal note) to an issue.",
            "inputSchema": {
                "type": "object",
                "required": ["id", "notes"],
                "properties": {
                    "id": { "type": "integer" },
                    "notes": { "type": "string" },
                    "private_notes": { "type": "boolean" }
                }
            }
        }),
        json!({
            "name": "redmine_list_users",
            "description": "List users (requires admin).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Filter by first/last/login name substring" },
                    "status": { "type": "integer", "description": "1=active, 2=registered, 3=locked" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
                    "offset": { "type": "integer", "minimum": 0 }
                }
            }
        }),
        json!({
            "name": "redmine_current_user",
            "description": "Get the currently authenticated (or impersonated) user.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "redmine_search",
            "description": "Full-text search across Redmine.",
            "inputSchema": {
                "type": "object",
                "required": ["q"],
                "properties": {
                    "q": { "type": "string", "description": "Search query" },
                    "scope": { "type": "string" },
                    "issues": { "type": "integer" },
                    "news": { "type": "integer" },
                    "documents": { "type": "integer" },
                    "wiki_pages": { "type": "integer" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
                    "offset": { "type": "integer", "minimum": 0 }
                }
            }
        }),
        json!({
            "name": "redmine_list_time_entries",
            "description": "List time entries with optional filters.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "user_id": { "type": "string" },
                    "project_id": { "type": "string", "description": "Project id, identifier, or display name" },
                    "issue_id": { "type": "integer" },
                    "from": { "type": "string", "description": "YYYY-MM-DD" },
                    "to": { "type": "string", "description": "YYYY-MM-DD" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
                    "offset": { "type": "integer", "minimum": 0 }
                }
            }
        }),
        json!({
            "name": "redmine_create_time_entry",
            "description": "Log time against an issue or project.",
            "inputSchema": {
                "type": "object",
                "required": ["hours"],
                "properties": {
                    "issue_id": { "type": "integer" },
                    "project_id": { "type": "string", "description": "Project id, identifier, or display name" },
                    "hours": { "type": "number" },
                    "spent_on": { "type": "string", "description": "YYYY-MM-DD (default: today)" },
                    "activity_id": { "type": "integer" },
                    "comments": { "type": "string" }
                }
            }
        }),
        json!({
            "name": "redmine_list_issue_attachments",
            "description": "List attachments on an issue.",
            "inputSchema": {
                "type": "object",
                "required": ["issue_id"],
                "properties": { "issue_id": { "type": "integer" } }
            }
        }),
        json!({
            "name": "redmine_get_attachment",
            "description": "Download a Redmine attachment by id. Images are returned as MCP image content; other types as base64. Optionally write raw bytes to save_to.",
            "inputSchema": {
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": { "type": "integer" },
                    "save_to": { "type": "string" },
                    "max_bytes": { "type": "integer" }
                }
            }
        }),
    ];

    if !lock_on_behalf_of {
        let prop = json!({
            "type": "string",
            "description": "Act on behalf of this Redmine user (login or email). Requires an admin API key."
        });
        for tool in &mut list {
            if let Some(props) = tool
                .get_mut("inputSchema")
                .and_then(|s| s.get_mut("properties"))
                .and_then(|p| p.as_object_mut())
            {
                props.insert("on_behalf_of".into(), prop.clone());
            }
        }
    }

    Value::Array(list)
}
