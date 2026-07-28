//! Core application: configuration, Redmine HTTP client, name-resolution
//! helpers, and the MCP tool implementations.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::{json, Map, Value};

fn truthy(s: &str) -> bool {
    matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes")
}

/// Turn a display name into a Redmine-style identifier: "Bluehive AI" -> "bluehive-ai".
pub fn slugify(value: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in value.to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

#[derive(Clone)]
pub struct Config {
    pub base_url: String,
    pub api_key: String,
    pub on_behalf_of: String,
    pub lock_on_behalf_of: bool,
    pub allow_admin: bool,
}

impl Config {
    pub fn from_env() -> Self {
        let base_url = std::env::var("REDMINE_URL")
            .unwrap_or_default()
            .trim_end_matches('/')
            .to_string();
        Config {
            base_url,
            api_key: std::env::var("REDMINE_API_KEY").unwrap_or_default(),
            on_behalf_of: std::env::var("REDMINE_ON_BEHALF_OF")
                .unwrap_or_default()
                .trim()
                .to_string(),
            lock_on_behalf_of: truthy(&std::env::var("REDMINE_LOCK_ON_BEHALF_OF").unwrap_or_default()),
            allow_admin: truthy(&std::env::var("REDMINE_ALLOW_ADMIN").unwrap_or_default()),
        }
    }
}

pub struct AppState {
    pub cfg: Config,
    http: reqwest::Client,
    admin: Mutex<Option<bool>>,
    login_cache: Mutex<HashMap<String, String>>,
    project_cache: Mutex<HashMap<String, String>>,
    user_cache: Mutex<HashMap<String, String>>,
    ref_cache: Mutex<HashMap<String, (Instant, Vec<Value>)>>,
}

const REF_TTL: Duration = Duration::from_secs(60);

impl AppState {
    pub fn new(cfg: Config) -> Self {
        AppState {
            cfg,
            http: reqwest::Client::builder()
                .user_agent("redmine-mcp-rs/0.1")
                .build()
                .expect("failed to build HTTP client"),
            admin: Mutex::new(None),
            login_cache: Mutex::new(HashMap::new()),
            project_cache: Mutex::new(HashMap::new()),
            user_cache: Mutex::new(HashMap::new()),
            ref_cache: Mutex::new(HashMap::new()),
        }
    }

    // -- HTTP -----------------------------------------------------------------

    async fn request(
        &self,
        method: &str,
        path: &str,
        switch_user: Option<&str>,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Value, String> {
        if self.cfg.base_url.is_empty() {
            return Err("REDMINE_URL is not configured".into());
        }
        if self.cfg.api_key.is_empty() {
            return Err("REDMINE_API_KEY is not configured".into());
        }
        let url = format!("{}{}", self.cfg.base_url, path);
        let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
        let mut req = self
            .http
            .request(m, &url)
            .header("X-Redmine-API-Key", &self.cfg.api_key)
            .header("Accept", "application/json");
        if let Some(su) = switch_user {
            req = req.header("X-Redmine-Switch-User", su);
        }
        if !query.is_empty() {
            req = req.query(query);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            if status.as_u16() == 412 {
                if let Some(su) = switch_user {
                    return Err(format!(
                        "Redmine impersonation failed: user '{su}' does not exist or is not active (X-Redmine-Switch-User returned 412)."
                    ));
                }
            }
            let snippet: String = text.chars().take(500).collect();
            return Err(format!("Redmine {method} {path} failed: {status} - {snippet}"));
        }
        if text.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).or_else(|_| Ok(json!({ "raw": text })))
    }

    async fn download(
        &self,
        url: &str,
        switch_user: Option<&str>,
    ) -> Result<(String, Vec<u8>), String> {
        let mut req = self.http.get(url).header("X-Redmine-API-Key", &self.cfg.api_key);
        if let Some(su) = switch_user {
            req = req.header("X-Redmine-Switch-User", su);
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("Redmine download {url} failed: {}", resp.status()));
        }
        let mime = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(';').next())
            .unwrap_or("application/octet-stream")
            .trim()
            .to_string();
        let bytes = resp.bytes().await.map_err(|e| e.to_string())?.to_vec();
        Ok((mime, bytes))
    }

    // -- Impersonation --------------------------------------------------------

    async fn ensure_admin(&self) -> bool {
        if let Some(b) = *self.admin.lock().unwrap() {
            return b;
        }
        let is = self
            .request("GET", "/users/current.json", None, &[], None)
            .await
            .ok()
            .and_then(|d| {
                d.get("user")
                    .and_then(|u| u.get("admin"))
                    .and_then(|a| a.as_bool())
            })
            .unwrap_or(false);
        *self.admin.lock().unwrap() = Some(is);
        is
    }

    /// Resolve an impersonation identity (login or email) to a Redmine login.
    async fn resolve_login(&self, identity: &str) -> Result<String, String> {
        let value = identity.trim().to_string();
        if value.is_empty() {
            return Err("empty impersonation identity".into());
        }
        if let Some(v) = self.login_cache.lock().unwrap().get(&value) {
            return Ok(v.clone());
        }
        if !value.contains('@') {
            self.login_cache.lock().unwrap().insert(value.clone(), value.clone());
            return Ok(value);
        }
        let data = self
            .request(
                "GET",
                "/users.json",
                None,
                &[("name".into(), value.clone()), ("limit".into(), "100".into())],
                None,
            )
            .await?;
        let login = data
            .get("users")
            .and_then(|u| u.as_array())
            .and_then(|users| {
                users.iter().find(|u| {
                    u.get("mail")
                        .and_then(|m| m.as_str())
                        .map(|m| m.eq_ignore_ascii_case(&value))
                        .unwrap_or(false)
                })
            })
            .and_then(|u| u.get("login").and_then(|l| l.as_str()))
            .map(|s| s.to_string());
        match login {
            Some(l) => {
                self.login_cache.lock().unwrap().insert(value, l.clone());
                Ok(l)
            }
            None => Err(format!(
                "Could not resolve '{value}' to a Redmine login (no active user with that email)."
            )),
        }
    }

    // -- Name resolution ------------------------------------------------------

    async fn load_ref(
        &self,
        scope: &str,
        cache_key: &str,
        path: &str,
        list_key: &str,
        su: Option<&str>,
    ) -> Vec<Value> {
        let key = format!("{scope}:{cache_key}");
        if let Some((t, v)) = self.ref_cache.lock().unwrap().get(&key) {
            if t.elapsed() < REF_TTL {
                return v.clone();
            }
        }
        let list = self
            .request("GET", path, su, &[], None)
            .await
            .ok()
            .and_then(|d| d.get(list_key).and_then(|x| x.as_array()).cloned())
            .unwrap_or_default();
        self.ref_cache
            .lock()
            .unwrap()
            .insert(key, (Instant::now(), list.clone()));
        list
    }

    fn id_string(v: &Value) -> Option<String> {
        v.get("id").map(|id| match id {
            Value::Number(n) => n.to_string(),
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
    }

    async fn resolve_enum(
        &self,
        scope: &str,
        cache_key: &str,
        path: &str,
        list_key: &str,
        value: &str,
        su: Option<&str>,
    ) -> String {
        let v = value.trim();
        if v.is_empty() || v.chars().all(|c| c.is_ascii_digit()) {
            return v.to_string();
        }
        let items = self.load_ref(scope, cache_key, path, list_key, su).await;
        for it in &items {
            if it
                .get("name")
                .and_then(|n| n.as_str())
                .map(|n| n.trim().eq_ignore_ascii_case(v))
                .unwrap_or(false)
            {
                if let Some(id) = Self::id_string(it) {
                    return id;
                }
            }
        }
        v.to_string()
    }

    async fn resolve_status(&self, scope: &str, value: &str, su: Option<&str>) -> String {
        let v = value.trim();
        if v.is_empty()
            || v.chars().all(|c| c.is_ascii_digit())
            || matches!(v.to_ascii_lowercase().as_str(), "open" | "closed" | "*")
        {
            return v.to_string();
        }
        self.resolve_enum(scope, "statuses", "/issue_statuses.json", "issue_statuses", v, su)
            .await
    }

    async fn project_by_ref(&self, reference: &str, su: Option<&str>) -> Option<Value> {
        self.request(
            "GET",
            &format!("/projects/{}.json", urlencode(reference)),
            su,
            &[],
            None,
        )
        .await
        .ok()
        .and_then(|d| d.get("project").cloned())
    }

    /// Resolve a project reference (numeric id, identifier, or display name) to a
    /// value the Redmine API accepts. Cheap: slug lookup + server-side name filter,
    /// cached per user.
    async fn resolve_project(&self, scope: &str, value: &str, su: Option<&str>) -> String {
        let v = value.trim();
        if v.is_empty() || v.chars().all(|c| c.is_ascii_digit()) {
            return v.to_string();
        }
        let ckey = format!("{scope}:{}", v.to_ascii_lowercase());
        if let Some(hit) = self.project_cache.lock().unwrap().get(&ckey) {
            return hit.clone();
        }
        let ident_or_id = |p: &Value| -> String {
            p.get("identifier")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
                .or_else(|| Self::id_string(p))
                .unwrap_or_else(|| v.to_string())
        };

        let slug = slugify(v);

        // 1. Slugified direct lookup (also catches values already an identifier).
        if !slug.is_empty() {
            if let Some(p) = self.project_by_ref(&slug, su).await {
                let r = ident_or_id(&p);
                self.project_cache.lock().unwrap().insert(ckey, r.clone());
                return r;
            }
        }

        // 2. Server-side name filter (single request even on large instances).
        if let Ok(data) = self
            .request(
                "GET",
                "/projects.json",
                su,
                &[("name".into(), v.to_string()), ("limit".into(), "100".into())],
                None,
            )
            .await
        {
            if let Some(projects) = data.get("projects").and_then(|p| p.as_array()) {
                let lower = v.to_ascii_lowercase();
                let by_name = |p: &&Value| {
                    p.get("name")
                        .and_then(|n| n.as_str())
                        .map(|n| n.trim().eq_ignore_ascii_case(&lower))
                        .unwrap_or(false)
                };
                let by_ident = |p: &&Value| {
                    p.get("identifier")
                        .and_then(|x| x.as_str())
                        .map(|x| x == v || x == slug)
                        .unwrap_or(false)
                };
                let matched = projects
                    .iter()
                    .find(by_name)
                    .or_else(|| projects.iter().find(by_ident))
                    .or_else(|| if projects.len() == 1 { projects.first() } else { None });
                if let Some(p) = matched {
                    let r = ident_or_id(p);
                    self.project_cache.lock().unwrap().insert(ckey, r.clone());
                    return r;
                }
            }
        }

        // 3. Last resort: page through every visible project.
        let all = self
            .load_ref_paged(scope, "projects", "/projects.json", "projects", su)
            .await;
        let lower = v.to_ascii_lowercase();
        let matched = all
            .iter()
            .find(|p| {
                p.get("name")
                    .and_then(|n| n.as_str())
                    .map(|n| n.trim().eq_ignore_ascii_case(&lower))
                    .unwrap_or(false)
            })
            .or_else(|| {
                all.iter().find(|p| {
                    p.get("identifier").and_then(|x| x.as_str()) == Some(slug.as_str())
                })
            });
        let r = matched.map(|p| ident_or_id(p)).unwrap_or_else(|| v.to_string());
        self.project_cache.lock().unwrap().insert(ckey, r.clone());
        r
    }

    async fn load_ref_paged(
        &self,
        scope: &str,
        cache_key: &str,
        path: &str,
        list_key: &str,
        su: Option<&str>,
    ) -> Vec<Value> {
        let key = format!("{scope}:{cache_key}:all");
        if let Some((t, v)) = self.ref_cache.lock().unwrap().get(&key) {
            if t.elapsed() < REF_TTL {
                return v.clone();
            }
        }
        let mut acc: Vec<Value> = Vec::new();
        let mut offset = 0usize;
        loop {
            let data = match self
                .request(
                    "GET",
                    path,
                    su,
                    &[
                        ("limit".into(), "100".into()),
                        ("offset".into(), offset.to_string()),
                    ],
                    None,
                )
                .await
            {
                Ok(d) => d,
                Err(_) => break,
            };
            let batch = data
                .get(list_key)
                .and_then(|x| x.as_array())
                .cloned()
                .unwrap_or_default();
            let total = data.get("total_count").and_then(|t| t.as_u64()).unwrap_or(0) as usize;
            let n = batch.len();
            acc.extend(batch);
            offset += n;
            if n == 0 || acc.len() >= total {
                break;
            }
        }
        self.ref_cache
            .lock()
            .unwrap()
            .insert(key, (Instant::now(), acc.clone()));
        acc
    }

    async fn resolve_tracker(&self, scope: &str, v: &str, su: Option<&str>) -> String {
        self.resolve_enum(scope, "trackers", "/trackers.json", "trackers", v, su)
            .await
    }

    async fn resolve_priority(&self, scope: &str, v: &str, su: Option<&str>) -> String {
        self.resolve_enum(
            scope,
            "priorities",
            "/enumerations/issue_priorities.json",
            "issue_priorities",
            v,
            su,
        )
        .await
    }

    /// Resolve a user reference (numeric id, 'me', login, email, or display name)
    /// to a numeric user id. Cached per scope. Falls back to the original value so
    /// the Redmine API can surface a clear error when no match is found.
    async fn resolve_user(&self, scope: &str, value: &str, su: Option<&str>) -> String {
        let v = value.trim();
        if v.is_empty() || v.chars().all(|c| c.is_ascii_digit()) || v.eq_ignore_ascii_case("me") {
            return v.to_string();
        }
        let ckey = format!("{scope}:{}", v.to_ascii_lowercase());
        if let Some(hit) = self.user_cache.lock().unwrap().get(&ckey) {
            return hit.clone();
        }
        // Search, then strict-match. Redmine's `?name=` is a loose token search, so
        // we never trust a lone fuzzy hit — only an exact login/email/full-name match.
        let mut users = self.search_users(v, su).await;
        if Self::match_user(&users, v).is_none() {
            // Redmine's multi-token name search is unreliable (e.g. "Raj Gara" can
            // miss the real user); retry on the last token (usually the surname).
            let tokens: Vec<&str> = v.split_whitespace().collect();
            if tokens.len() > 1 {
                users = self.search_users(tokens[tokens.len() - 1], su).await;
            }
        }
        if let Some(id) = Self::match_user(&users, v) {
            self.user_cache.lock().unwrap().insert(ckey, id.clone());
            return id;
        }
        v.to_string()
    }

    /// Fetch users via `/users.json?name=`. Listing users needs admin rights, so if
    /// the impersonated (often non-admin) user is denied, retry with the admin key.
    async fn search_users(&self, query: &str, su: Option<&str>) -> Vec<Value> {
        let q = [
            ("name".to_string(), query.to_string()),
            ("limit".to_string(), "100".to_string()),
        ];
        let extract = |d: Option<Value>| -> Vec<Value> {
            d.and_then(|x| x.get("users").and_then(|u| u.as_array()).cloned())
                .unwrap_or_default()
        };
        let mut users = extract(self.request("GET", "/users.json", su, &q, None).await.ok());
        if su.is_some() && users.is_empty() {
            users = extract(self.request("GET", "/users.json", None, &q, None).await.ok());
        }
        users
    }

    /// Strict match: login, email, or exact "firstname lastname" (case-insensitive).
    /// No single-result guessing — Redmine's name search is too loose to trust blindly.
    fn match_user(users: &[Value], value: &str) -> Option<String> {
        let lower = value.trim().to_ascii_lowercase();
        let field_eq = |u: &&Value, key: &str| {
            u.get(key)
                .and_then(|x| x.as_str())
                .map(|s| s.trim().eq_ignore_ascii_case(&lower))
                .unwrap_or(false)
        };
        let full_name_eq = |u: &&Value| {
            let f = u.get("firstname").and_then(|x| x.as_str()).unwrap_or("");
            let l = u.get("lastname").and_then(|x| x.as_str()).unwrap_or("");
            format!("{f} {l}").trim().eq_ignore_ascii_case(&lower)
        };
        users
            .iter()
            .find(|u| field_eq(u, "login"))
            .or_else(|| users.iter().find(|u| field_eq(u, "mail")))
            .or_else(|| users.iter().find(full_name_eq))
            .and_then(Self::id_string)
    }

    // -- Tool dispatch --------------------------------------------------------

    /// Entry point from the protocol layer for a `tools/call`.
    pub async fn call_tool(&self, name: &str, mut args: Map<String, Value>) -> Value {
        // Resolve impersonation identity (mirrors the JS server semantics).
        let per_call = args
            .remove("on_behalf_of")
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        let identity = if self.cfg.lock_on_behalf_of {
            self.cfg.on_behalf_of.clone()
        } else if !per_call.is_empty() {
            per_call
        } else {
            self.cfg.on_behalf_of.clone()
        };

        let result = if !identity.is_empty() {
            if self.ensure_admin().await {
                match self.resolve_login(&identity).await {
                    Ok(login) => self.handle_tool(name, args, Some(login)).await,
                    Err(e) => Err(e),
                }
            } else {
                self.handle_tool(name, args, None).await
            }
        } else if !self.cfg.allow_admin && self.ensure_admin().await {
            Err(
                "Refusing to run with an admin API key and no impersonation identity. Set \
                 REDMINE_ON_BEHALF_OF (and REDMINE_LOCK_ON_BEHALF_OF=1 for shared deployments) to \
                 attribute actions to a specific user, or set REDMINE_ALLOW_ADMIN=1 to \
                 intentionally act as the admin key owner."
                    .to_string(),
            )
        } else {
            self.handle_tool(name, args, None).await
        };

        match result {
            Ok(v) => v,
            Err(e) => json!({
                "isError": true,
                "content": [{ "type": "text", "text": format!("Error: {e}") }],
            }),
        }
    }

    async fn handle_tool(
        &self,
        name: &str,
        mut args: Map<String, Value>,
        switch_user: Option<String>,
    ) -> Result<Value, String> {
        let su = switch_user.as_deref();
        let scope = su.unwrap_or("");

        match name {
            "redmine_list_projects" => {
                let q = query_from_value(&args);
                ok(self.request("GET", "/projects.json", su, &q, None).await?)
            }

            "redmine_get_project" => {
                let id = str_arg(&args, "id");
                let resolved = self.resolve_project(scope, &id, su).await;
                ok(self
                    .request("GET", &format!("/projects/{}.json", urlencode(&resolved)), su, &[], None)
                    .await?)
            }

            "redmine_list_issues" => {
                self.resolve_issue_filters(&mut args, scope, su).await;
                let q = query_from_value(&args);
                ok(self.request("GET", "/issues.json", su, &q, None).await?)
            }

            "redmine_get_issue" => {
                let id = str_arg(&args, "id");
                let include = args
                    .get("include")
                    .and_then(|v| v.as_str())
                    .unwrap_or("journals,attachments,children,relations,watchers")
                    .to_string();
                ok(self
                    .request(
                        "GET",
                        &format!("/issues/{id}.json"),
                        su,
                        &[("include".into(), include)],
                        None,
                    )
                    .await?)
            }

            "redmine_create_issue" => {
                self.resolve_issue_filters(&mut args, scope, su).await;
                if let Some(p) = args.get("project_id").cloned() {
                    let r = self.resolve_project(scope, &value_to_string(&p), su).await;
                    args.insert("project_id".into(), Value::String(r));
                }
                let body = json!({ "issue": Value::Object(args) });
                ok(self.request("POST", "/issues.json", su, &[], Some(&body)).await?)
            }

            "redmine_update_issue" => {
                let id = str_arg(&args, "id");
                args.remove("id");
                self.resolve_issue_filters(&mut args, scope, su).await;
                let body = json!({ "issue": Value::Object(args) });
                self.request("PUT", &format!("/issues/{id}.json"), su, &[], Some(&body))
                    .await?;
                ok(json!({ "ok": true, "id": id }))
            }

            "redmine_add_issue_note" => {
                let id = str_arg(&args, "id");
                let mut issue = Map::new();
                if let Some(n) = args.get("notes") {
                    issue.insert("notes".into(), n.clone());
                }
                if let Some(p) = args.get("private_notes") {
                    issue.insert("private_notes".into(), p.clone());
                }
                let body = json!({ "issue": Value::Object(issue) });
                self.request("PUT", &format!("/issues/{id}.json"), su, &[], Some(&body))
                    .await?;
                ok(json!({ "ok": true, "id": id }))
            }

            "redmine_list_users" => {
                let q = query_from_value(&args);
                ok(self.request("GET", "/users.json", su, &q, None).await?)
            }

            "redmine_current_user" => {
                ok(self.request("GET", "/users/current.json", su, &[], None).await?)
            }

            "redmine_search" => {
                let q = query_from_value(&args);
                ok(self.request("GET", "/search.json", su, &q, None).await?)
            }

            "redmine_list_time_entries" => {
                if let Some(p) = args.get("project_id").cloned() {
                    let r = self.resolve_project(scope, &value_to_string(&p), su).await;
                    args.insert("project_id".into(), Value::String(r));
                }
                let q = query_from_value(&args);
                ok(self.request("GET", "/time_entries.json", su, &q, None).await?)
            }

            "redmine_create_time_entry" => {
                if let Some(p) = args.get("project_id").cloned() {
                    let r = self.resolve_project(scope, &value_to_string(&p), su).await;
                    args.insert("project_id".into(), Value::String(r));
                }
                let body = json!({ "time_entry": Value::Object(args) });
                ok(self
                    .request("POST", "/time_entries.json", su, &[], Some(&body))
                    .await?)
            }

            "redmine_list_issue_attachments" => {
                let issue_id = str_arg(&args, "issue_id");
                let data = self
                    .request(
                        "GET",
                        &format!("/issues/{issue_id}.json"),
                        su,
                        &[("include".into(), "attachments".into())],
                        None,
                    )
                    .await?;
                let atts = data
                    .get("issue")
                    .and_then(|i| i.get("attachments"))
                    .and_then(|a| a.as_array())
                    .cloned()
                    .unwrap_or_default();
                ok(json!({
                    "issue_id": issue_id,
                    "count": atts.len(),
                    "attachments": atts,
                }))
            }

            "redmine_get_attachment" => self.get_attachment(&args, su).await,

            other => Err(format!("Unknown tool: {other}")),
        }
    }

    async fn resolve_issue_filters(&self, args: &mut Map<String, Value>, scope: &str, su: Option<&str>) {
        if let Some(p) = args.get("project_id").cloned() {
            let s = value_to_string(&p);
            if !s.is_empty() {
                let r = self.resolve_project(scope, &s, su).await;
                args.insert("project_id".into(), Value::String(r));
            }
        }
        if let Some(t) = args.get("tracker_id").cloned() {
            let s = value_to_string(&t);
            if !s.is_empty() && !s.chars().all(|c| c.is_ascii_digit()) {
                let r = self.resolve_tracker(scope, &s, su).await;
                args.insert("tracker_id".into(), Value::String(r));
            }
        }
        if let Some(p) = args.get("priority_id").cloned() {
            let s = value_to_string(&p);
            if !s.is_empty() && !s.chars().all(|c| c.is_ascii_digit()) {
                let r = self.resolve_priority(scope, &s, su).await;
                args.insert("priority_id".into(), Value::String(r));
            }
        }
        if let Some(st) = args.get("status_id").cloned() {
            let s = value_to_string(&st);
            if !s.is_empty() {
                let r = self.resolve_status(scope, &s, su).await;
                args.insert("status_id".into(), Value::String(r));
            }
        }
        for key in ["assigned_to_id", "author_id"] {
            if let Some(u) = args.get(key).cloned() {
                let s = value_to_string(&u);
                if !s.is_empty()
                    && !s.chars().all(|c| c.is_ascii_digit())
                    && !s.eq_ignore_ascii_case("me")
                {
                    let r = self.resolve_user(scope, &s, su).await;
                    args.insert(key.to_string(), Value::String(r));
                }
            }
        }
    }

    async fn get_attachment(&self, args: &Map<String, Value>, su: Option<&str>) -> Result<Value, String> {
        let id = str_arg(args, "id");
        let meta = self
            .request("GET", &format!("/attachments/{id}.json"), su, &[], None)
            .await?;
        let att = meta
            .get("attachment")
            .cloned()
            .ok_or_else(|| format!("Attachment {id} not found"))?;
        let content_url = att
            .get("content_url")
            .and_then(|v| v.as_str())
            .ok_or("attachment has no content_url")?;
        let max_bytes = args
            .get("max_bytes")
            .and_then(|v| v.as_u64())
            .unwrap_or(10 * 1024 * 1024) as usize;

        let (mime, bytes) = self.download(content_url, su).await?;

        let mut saved_to: Option<String> = None;
        if let Some(dest) = args.get("save_to").and_then(|v| v.as_str()) {
            if let Some(parent) = std::path::Path::new(dest).parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            tokio::fs::write(dest, &bytes).await.map_err(|e| e.to_string())?;
            saved_to = Some(dest.to_string());
        }

        let info = json!({
            "id": att.get("id"),
            "filename": att.get("filename"),
            "content_type": att.get("content_type").cloned().unwrap_or_else(|| Value::String(mime.clone())),
            "filesize": att.get("filesize").cloned().unwrap_or_else(|| json!(bytes.len())),
            "description": att.get("description"),
            "author": att.get("author"),
            "created_on": att.get("created_on"),
            "saved_to": saved_to,
        });

        if bytes.len() > max_bytes {
            let mut m = info.as_object().unwrap().clone();
            m.insert(
                "note".into(),
                Value::String(format!(
                    "Attachment is {} bytes which exceeds max_bytes={max_bytes}.",
                    bytes.len()
                )),
            );
            m.insert("inlined".into(), Value::Bool(false));
            return ok(Value::Object(m));
        }

        let b64 = B64.encode(&bytes);
        let is_image = matches!(
            mime.to_ascii_lowercase().as_str(),
            "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp"
        );
        if is_image {
            return Ok(json!({
                "content": [
                    { "type": "text", "text": serde_json::to_string_pretty(&info).unwrap() },
                    { "type": "image", "data": b64, "mimeType": mime },
                ]
            }));
        }
        let mut m = info.as_object().unwrap().clone();
        m.insert("base64".into(), Value::String(b64));
        m.insert("inlined".into(), Value::Bool(true));
        ok(Value::Object(m))
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

fn str_arg(args: &Map<String, Value>, key: &str) -> String {
    args.get(key).map(value_to_string).unwrap_or_default()
}

/// Build query pairs from a JSON object, skipping null, empty strings and
/// numeric-zero filters (LLMs fill optional tracker_id/priority_id with 0,
/// which silently corrupts Redmine queries). `offset` may legitimately be 0.
fn query_from_value(v: &Map<String, Value>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (k, val) in v {
        match val {
            Value::Null => continue,
            Value::String(s) => {
                if s.is_empty() {
                    continue;
                }
                out.push((k.clone(), s.clone()));
            }
            Value::Number(n) => {
                if n.as_f64() == Some(0.0) && k != "offset" {
                    continue;
                }
                out.push((k.clone(), n.to_string()));
            }
            Value::Bool(b) => out.push((k.clone(), b.to_string())),
            other => out.push((k.clone(), other.to_string())),
        }
    }
    out
}

/// Wrap arbitrary JSON data as a successful MCP tool result.
fn ok(data: Value) -> Result<Value, String> {
    Ok(json!({
        "content": [
            { "type": "text", "text": serde_json::to_string_pretty(&data).unwrap_or_default() }
        ]
    }))
}
