//! A small GitHub REST/GraphQL client over `fetch`, authenticated with the
//! short-lived user token from the worker's `/auth/token`.

use std::cell::RefCell;
use std::collections::HashMap;

use anyhow::{Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::Deserialize;
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestCache, RequestInit, Response};

const API: &str = "https://api.github.com";

/// A response with a non-2xx status.
#[derive(Debug)]
pub struct HttpError {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GitHub: {} ({})", self.message, self.status)
    }
}

impl std::error::Error for HttpError {}

pub fn status(e: &anyhow::Error) -> Option<u16> {
    e.downcast_ref::<HttpError>().map(|e| e.status)
}

async fn http(method: &str, url: &str, headers: &[(&str, &str)], body: Option<&str>) -> Result<(u16, String)> {
    let js = |e: wasm_bindgen::JsValue| anyhow!("network error: {e:?}");
    let init = RequestInit::new();
    init.set_method(method);
    init.set_cache(RequestCache::NoStore);
    if let Some(b) = body {
        init.set_body(&b.into());
    }
    let req = Request::new_with_str_and_init(url, &init).map_err(js)?;
    for (k, v) in headers {
        req.headers().set(k, v).map_err(js)?;
    }
    let window = web_sys::window().ok_or_else(|| anyhow!("no window"))?;
    let resp: Response = JsFuture::from(window.fetch_with_request(&req)).await.map_err(js)?.unchecked_into();
    let text = JsFuture::from(resp.text().map_err(js)?).await.map_err(js)?.as_string().unwrap_or_default();
    Ok((resp.status(), text))
}

#[derive(Debug, Clone, Deserialize)]
pub struct Session {
    pub token: String,
    /// Unix seconds; `None` for tokens that don't expire.
    pub expires_at: Option<f64>,
    /// The GitHub App's slug, for the install link.
    pub app: Option<String>,
}

/// Asks the worker for a user token. `None` when not signed in.
pub async fn session() -> Result<Option<Session>> {
    let (status, body) = http("GET", "/auth/token", &[], None).await?;
    match status {
        200 => Ok(Some(serde_json::from_str(&body)?)),
        401 => Ok(None),
        s => bail!("sign-in service error ({s}): {body}"),
    }
}

/// Asks the worker for the title of the page at `url`.
pub async fn page_title(url: &str) -> Option<String> {
    let url = format!("/auth/title?url={}", String::from(js_sys::encode_uri_component(url)));
    let (status, body) = http("GET", &url, &[], None).await.ok()?;
    let v: Value = serde_json::from_str(&body).ok().filter(|_| status == 200)?;
    v["title"].as_str().map(str::to_string)
}

fn now_secs() -> f64 {
    js_sys::Date::now() / 1000.0
}

pub struct Api {
    pub owner: String,
    pub repo: String,
    session: RefCell<Session>,
}

pub struct TreeEntry {
    pub path: String,
    pub mode: String,
    pub sha: String,
}

impl Api {
    pub fn new(owner: &str, repo: &str, session: Session) -> Self {
        Api { owner: owner.into(), repo: repo.into(), session: RefCell::new(session) }
    }

    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    async fn token(&self) -> Result<String> {
        let expiring = self.session.borrow().expires_at.is_some_and(|t| t - now_secs() < 120.0);
        if expiring {
            let s = session().await?.ok_or_else(|| anyhow!("signed out; reload the page to sign in"))?;
            *self.session.borrow_mut() = s;
        }
        Ok(self.session.borrow().token.clone())
    }

    /// Calls the REST API. `path` starts with `/`; `{repo}` expands to `/repos/owner/name`.
    pub async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let url = format!("{API}{}", path.replace("{repo}", &format!("/repos/{}/{}", self.owner, self.repo)));
        let auth = format!("Bearer {}", self.token().await?);
        let body = body.map(|b| b.to_string());
        let headers = [
            ("Authorization", auth.as_str()),
            ("Accept", "application/vnd.github+json"),
            ("X-GitHub-Api-Version", "2022-11-28"),
        ];
        let (status, text) = http(method, &url, &headers, body.as_deref()).await?;
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            let message = value["message"].as_str().unwrap_or("request failed").to_string();
            return Err(HttpError { status, message }.into());
        }
        Ok(value)
    }

    pub async fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let auth = format!("Bearer {}", self.token().await?);
        let body = json!({ "query": query, "variables": variables }).to_string();
        let (status, text) =
            http("POST", &format!("{API}/graphql"), &[("Authorization", auth.as_str())], Some(&body)).await?;
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if status != 200 {
            return Err(HttpError { status, message: value["message"].as_str().unwrap_or("").into() }.into());
        }
        if let Some(err) = value["errors"].get(0) {
            bail!("GitHub: {}", err["message"].as_str().unwrap_or("GraphQL error"));
        }
        Ok(value["data"].clone())
    }

    /// Repos the signed-in user installed the app on, as `owner/name`.
    pub async fn installed_repos(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let installs = self.call("GET", "/user/installations?per_page=100", None).await?;
        for inst in installs["installations"].as_array().into_iter().flatten() {
            let id = inst["id"].as_u64().unwrap_or_default();
            for page in 1..=10 {
                let repos = self
                    .call("GET", &format!("/user/installations/{id}/repositories?per_page=100&page={page}"), None)
                    .await?;
                let list = repos["repositories"].as_array().cloned().unwrap_or_default();
                out.extend(list.iter().filter_map(|r| r["full_name"].as_str().map(str::to_string)));
                if list.len() < 100 {
                    break;
                }
            }
        }
        out.sort_by_key(|s| s.to_lowercase());
        out.dedup();
        Ok(out)
    }

    pub async fn default_branch(&self) -> Result<String> {
        let repo = self.call("GET", "{repo}", None).await?;
        Ok(repo["default_branch"].as_str().unwrap_or("main").to_string())
    }

    /// The commit a branch points to; `None` for an empty repo.
    pub async fn branch_head(&self, branch: &str) -> Result<Option<String>> {
        match self.call("GET", &format!("{{repo}}/git/ref/heads/{branch}"), None).await {
            Ok(v) => Ok(v["object"]["sha"].as_str().map(str::to_string)),
            Err(e) if status(&e) == Some(409) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Commits `head` is behind `remote` by.
    pub async fn behind_by(&self, head: &str, remote: &str) -> Result<usize> {
        let v = self.call("GET", &format!("{{repo}}/compare/{head}...{remote}"), None).await?;
        Ok(v["ahead_by"].as_u64().unwrap_or(1) as usize)
    }

    /// The tree of a commit: (tree sha, blob entries).
    pub async fn tree(&self, commit: &str) -> Result<(String, Vec<TreeEntry>)> {
        let c = self.call("GET", &format!("{{repo}}/git/commits/{commit}"), None).await?;
        let tree = c["tree"]["sha"].as_str().ok_or_else(|| anyhow!("commit has no tree"))?.to_string();
        let t = self.call("GET", &format!("{{repo}}/git/trees/{tree}?recursive=1"), None).await?;
        if t["truncated"].as_bool() == Some(true) {
            bail!("this repository is too large to open in the browser");
        }
        let entries = t["tree"]
            .as_array()
            .into_iter()
            .flatten()
            // Plain and executable files only; no symlinks or submodules.
            .filter(|e| e["type"] == "blob" && matches!(e["mode"].as_str(), Some("100644" | "100755")))
            .filter_map(|e| {
                Some(TreeEntry {
                    path: e["path"].as_str()?.into(),
                    mode: e["mode"].as_str()?.into(),
                    sha: e["sha"].as_str()?.into(),
                })
            })
            .collect();
        Ok((tree, entries))
    }

    /// Downloads blobs as text. Binary blobs are left out of the result.
    pub async fn texts(&self, shas: &[String]) -> Result<HashMap<String, Vec<u8>>> {
        let mut out = HashMap::new();
        for chunk in shas.chunks(100) {
            let fields: String = chunk
                .iter()
                .enumerate()
                .map(|(i, sha)| {
                    format!("b{i}: object(oid: \"{sha}\") {{ ... on Blob {{ text isBinary isTruncated }} }}\n")
                })
                .collect();
            let query = format!(
                "query($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{ {fields} }} }}"
            );
            let data = self.graphql(&query, json!({ "owner": self.owner, "name": self.repo })).await?;
            for (i, sha) in chunk.iter().enumerate() {
                let b = &data["repository"][format!("b{i}")];
                if b["isBinary"].as_bool() != Some(false) {
                    continue;
                }
                if b["isTruncated"].as_bool() == Some(true) {
                    out.insert(sha.clone(), self.blob(sha).await?);
                } else if let Some(t) = b["text"].as_str() {
                    out.insert(sha.clone(), t.as_bytes().to_vec());
                }
            }
        }
        Ok(out)
    }

    pub async fn blob(&self, sha: &str) -> Result<Vec<u8>> {
        let v = self.call("GET", &format!("{{repo}}/git/blobs/{sha}"), None).await?;
        let content: String = v["content"].as_str().unwrap_or_default().split_whitespace().collect();
        Ok(B64.decode(content)?)
    }

    pub async fn create_blob(&self, data: &[u8]) -> Result<String> {
        let v = self
            .call("POST", "{repo}/git/blobs", Some(json!({ "content": B64.encode(data), "encoding": "base64" })))
            .await?;
        sha(&v)
    }

    pub async fn create_tree(&self, base_tree: &str, entries: Vec<Value>) -> Result<String> {
        let v = self.call("POST", "{repo}/git/trees", Some(json!({ "base_tree": base_tree, "tree": entries }))).await?;
        sha(&v)
    }

    pub async fn create_commit(&self, message: &str, tree: &str, parent: &str) -> Result<String> {
        let body = json!({ "message": message, "tree": tree, "parents": [parent] });
        sha(&self.call("POST", "{repo}/git/commits", Some(body)).await?)
    }

    /// Moves a branch to `commit`; fails with 422 if that isn't a fast-forward.
    pub async fn update_branch(&self, branch: &str, commit: &str) -> Result<()> {
        let body = json!({ "sha": commit, "force": false });
        self.call("PATCH", &format!("{{repo}}/git/refs/heads/{branch}"), Some(body)).await?;
        Ok(())
    }

    /// Creates the first commit of an empty repo with one file.
    pub async fn create_file(&self, path: &str, content: &str, message: &str) -> Result<()> {
        let body = json!({ "message": message, "content": B64.encode(content) });
        self.call("PUT", &format!("{{repo}}/contents/{path}"), Some(body)).await?;
        Ok(())
    }
}

fn sha(v: &Value) -> Result<String> {
    v["sha"].as_str().map(str::to_string).ok_or_else(|| anyhow!("GitHub returned no sha"))
}
