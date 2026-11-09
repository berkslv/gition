//! A small GitHub REST/GraphQL client over `fetch`, authenticated with the
//! short-lived user token from the worker's `/auth/token`.

use std::cell::RefCell;
use std::collections::HashMap;

use anyhow::{Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use futures_util::{StreamExt, TryStreamExt, stream};
use serde::Deserialize;
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestCache, RequestInit, Response};

const API: &str = "https://api.github.com";

/// Requests [`Api::texts`] keeps in flight at once. Modest, so bursts stay clear of
/// GitHub's secondary rate limits.
const PARALLEL: usize = 4;

/// Blobs asked for in one GraphQL query.
const BLOBS_PER_QUERY: usize = 100;

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
    // fetch only rejects when the request never got an answer.
    let resp: Response = JsFuture::from(window.fetch_with_request(&req))
        .await
        .map_err(|_| anyhow!("can't reach GitHub, check your connection"))?
        .unchecked_into();
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

/// A commit, with the sha of its tree.
#[derive(Debug, Clone)]
pub struct Head {
    pub commit: String,
    pub tree: String,
}

impl Head {
    /// Reads a GraphQL `target { oid ... on Commit { tree { oid } } }`.
    fn from_target(target: &Value) -> Option<Head> {
        Some(Head { commit: target["oid"].as_str()?.into(), tree: target["tree"]["oid"].as_str()?.into() })
    }
}

/// The GraphQL fields [`Head::from_target`] reads.
const TARGET: &str = "target { oid ... on Commit { tree { oid } } }";

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

    /// The token, refreshed from the worker when it's about to expire. Requests that run
    /// together may each refresh it; the borrow is never held across the wait.
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
            // Like REST's 404, for repos that don't exist or that the app can't see.
            if err["type"] == "NOT_FOUND" {
                let message = err["message"].as_str().unwrap_or("Not Found").into();
                return Err(HttpError { status: 404, message }.into());
            }
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

    /// The default branch and its head; the head is `None` for an empty repo.
    /// Fails with a 404 [`HttpError`] if the repo isn't there or the app can't see it.
    pub async fn default_branch(&self) -> Result<(String, Option<Head>)> {
        let query = format!(
            "query($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{ \
             defaultBranchRef {{ name {TARGET} }} }} }}"
        );
        let data = self.graphql(&query, json!({ "owner": self.owner, "name": self.repo })).await?;
        let branch = &data["repository"]["defaultBranchRef"];
        // An empty repo has no default branch yet; it gets one with the first commit.
        let name = branch["name"].as_str().unwrap_or("main").to_string();
        Ok((name, Head::from_target(&branch["target"])))
    }

    /// The commit a branch points to; `None` if there's no such branch, as in an empty repo.
    pub async fn branch_head(&self, branch: &str) -> Result<Option<Head>> {
        let query = format!(
            "query($owner: String!, $name: String!, $ref: String!) {{ repository(owner: $owner, name: $name) {{ \
             ref(qualifiedName: $ref) {{ {TARGET} }} }} }}"
        );
        let vars = json!({ "owner": self.owner, "name": self.repo, "ref": format!("refs/heads/{branch}") });
        let data = self.graphql(&query, vars).await?;
        Ok(Head::from_target(&data["repository"]["ref"]["target"]))
    }

    /// Commits `head` is behind `remote` by.
    pub async fn behind_by(&self, head: &str, remote: &str) -> Result<usize> {
        let v = self.call("GET", &format!("{{repo}}/compare/{head}...{remote}"), None).await?;
        Ok(v["ahead_by"].as_u64().unwrap_or(1) as usize)
    }

    /// The tree of a commit: (tree sha, blob entries). Pass `tree` when it's known,
    /// to skip looking up the commit.
    pub async fn tree(&self, commit: &str, tree: Option<&str>) -> Result<(String, Vec<TreeEntry>)> {
        let tree = match tree {
            Some(t) => t.to_string(),
            None => {
                let c = self.call("GET", &format!("{{repo}}/git/commits/{commit}"), None).await?;
                c["tree"]["sha"].as_str().ok_or_else(|| anyhow!("commit has no tree"))?.to_string()
            }
        };
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
        let chunks: Vec<_> = stream::iter(shas.chunks(BLOBS_PER_QUERY))
            .map(|chunk| self.text_chunk(chunk))
            .buffer_unordered(PARALLEL)
            .try_collect()
            .await?;
        let mut out = HashMap::new();
        let mut truncated = Vec::new();
        for (texts, big) in chunks {
            out.extend(texts);
            truncated.extend(big);
        }
        // GraphQL cuts long texts short; the REST API has them whole.
        let big: Vec<_> = stream::iter(truncated)
            .map(|sha| async move {
                let data = self.blob(&sha).await?;
                Ok::<_, anyhow::Error>((sha, data))
            })
            .buffer_unordered(PARALLEL)
            .try_collect()
            .await?;
        out.extend(big);
        Ok(out)
    }

    /// One GraphQL query's worth of [`Api::texts`]: the texts, and the shas of truncated ones.
    async fn text_chunk(&self, chunk: &[String]) -> Result<(HashMap<String, Vec<u8>>, Vec<String>)> {
        let fields: String = chunk
            .iter()
            .enumerate()
            .map(|(i, sha)| format!("b{i}: object(oid: \"{sha}\") {{ ... on Blob {{ text isBinary isTruncated }} }}\n"))
            .collect();
        let query = format!(
            "query($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{ {fields} }} }}"
        );
        let data = self.graphql(&query, json!({ "owner": self.owner, "name": self.repo })).await?;
        let mut texts = HashMap::new();
        let mut truncated = Vec::new();
        for (i, sha) in chunk.iter().enumerate() {
            let b = &data["repository"][format!("b{i}")];
            if b["isBinary"].as_bool() != Some(false) {
                continue;
            }
            if b["isTruncated"].as_bool() == Some(true) {
                truncated.push(sha.clone());
            } else if let Some(t) = b["text"].as_str() {
                texts.insert(sha.clone(), t.as_bytes().to_vec());
            }
        }
        Ok((texts, truncated))
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
