//! The only server-side part of gition web: GitHub sign-in.
//!
//! Static files are served by Workers Static Assets; requests that don't match a
//! file land here. The worker keeps no state. The user's tokens live in an
//! encrypted, HttpOnly cookie, and the browser only ever gets the short-lived
//! access token, which it uses to talk to api.github.com directly.
//!
//! Config: vars `GITHUB_CLIENT_ID`, `GITHUB_APP_SLUG`; secrets `GITHUB_CLIENT_SECRET`, `SESSION_KEY`.

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use worker::*;

const SESSION: &str = "gition_session";
const STATE: &str = "gition_state";
/// Hand out a cached access token only if it lives at least this long (ms).
const MIN_TOKEN_LIFE: f64 = 10.0 * 60.0 * 1000.0;

#[derive(Serialize, Deserialize)]
struct Session {
    /// Access token and its expiry (ms since epoch; none for non-expiring tokens).
    access: String,
    access_exp: Option<f64>,
    refresh: Option<String>,
    refresh_exp: Option<f64>,
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let url = req.url()?;
    let res = match url.path() {
        "/auth/login" => login(&url, &env),
        "/auth/callback" => callback(&req, &url, &env).await,
        "/auth/token" => token(&req, &env).await,
        "/auth/logout" => redirect("/", &[cookie(SESSION, "", 0)]),
        _ => Response::error("Not found", 404),
    };
    res.or_else(|e| {
        console_error!("{e}");
        Response::error("Sign-in failed, try again", 500)
    })
}

fn now() -> f64 {
    Date::now().as_millis() as f64
}

fn cookie(name: &str, value: &str, max_age: u64) -> String {
    format!("{name}={value}; Path=/auth; HttpOnly; Secure; SameSite=Lax; Max-Age={max_age}")
}

fn get_cookie(req: &Request, name: &str) -> Option<String> {
    let header = req.headers().get("Cookie").ok()??;
    header.split(';').find_map(|c| c.trim().strip_prefix(name)?.strip_prefix('=').map(str::to_string))
}

fn redirect(location: &str, cookies: &[String]) -> Result<Response> {
    let headers = Headers::new();
    headers.set("Location", location)?;
    headers.set("Cache-Control", "no-store")?;
    for c in cookies {
        headers.append("Set-Cookie", c)?;
    }
    Ok(Response::empty()?.with_status(302).with_headers(headers))
}

fn origin(url: &Url) -> String {
    url.origin().ascii_serialization()
}

fn login(url: &Url, env: &Env) -> Result<Response> {
    let mut state = [0u8; 16];
    getrandom::getrandom(&mut state).map_err(|e| Error::RustError(e.to_string()))?;
    let state = B64.encode(state);
    let mut authorize = Url::parse("https://github.com/login/oauth/authorize")?;
    authorize
        .query_pairs_mut()
        .append_pair("client_id", &env.var("GITHUB_CLIENT_ID")?.to_string())
        .append_pair("redirect_uri", &format!("{}/auth/callback", origin(url)))
        .append_pair("state", &state);
    redirect(authorize.as_str(), &[cookie(STATE, &state, 600)])
}

async fn callback(req: &Request, url: &Url, env: &Env) -> Result<Response> {
    let param = |k: &str| url.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.into_owned());
    let (Some(code), Some(state)) = (param("code"), param("state")) else {
        return redirect("/", &[]);
    };
    if get_cookie(req, STATE).as_deref() != Some(state.as_str()) {
        return Response::error("Sign-in expired, go back and try again", 400);
    }
    let redirect_uri = format!("{}/auth/callback", origin(url));
    let tokens = exchange(env, &[("code", &code), ("redirect_uri", &redirect_uri)]).await?;
    let session = session_from(&tokens)?;
    redirect("/", &[cookie(STATE, "", 0), session_cookie(env, &session)?])
}

async fn token(req: &Request, env: &Env) -> Result<Response> {
    let unauthorized = || -> Result<Response> {
        let mut res = Response::error("Not signed in", 401)?;
        res.headers_mut().append("Set-Cookie", &cookie(SESSION, "", 0))?;
        Ok(res)
    };
    let Some(mut session) = get_cookie(req, SESSION).and_then(|c| open(env, &c).ok()) else {
        return unauthorized();
    };
    let mut set_cookie = None;
    if session.access_exp.is_some_and(|exp| exp - now() < MIN_TOKEN_LIFE) {
        let Some(refresh) = session.refresh.clone().filter(|_| session.refresh_exp.is_none_or(|e| e > now())) else {
            return unauthorized();
        };
        match exchange(env, &[("grant_type", "refresh_token"), ("refresh_token", &refresh)]).await {
            Ok(tokens) => session = session_from(&tokens)?,
            Err(_) => return unauthorized(),
        }
        set_cookie = Some(session_cookie(env, &session)?);
    }
    let body = json!({
        "token": session.access,
        "expires_at": session.access_exp.map(|ms| (ms / 1000.0).floor()),
        "app": env.var("GITHUB_APP_SLUG").map(|v| v.to_string()).ok(),
    });
    let mut res = Response::from_json(&body)?;
    res.headers_mut().set("Cache-Control", "no-store")?;
    if let Some(c) = set_cookie {
        res.headers_mut().append("Set-Cookie", &c)?;
    }
    Ok(res)
}

/// Calls GitHub's token endpoint with the app credentials plus `params`.
async fn exchange(env: &Env, params: &[(&str, &str)]) -> Result<Value> {
    let client_id = env.var("GITHUB_CLIENT_ID")?.to_string();
    let secret = env.secret("GITHUB_CLIENT_SECRET")?.to_string();
    let mut form = Url::parse("http://form")?;
    form.query_pairs_mut().append_pair("client_id", &client_id).append_pair("client_secret", &secret);
    for (k, v) in params {
        form.query_pairs_mut().append_pair(k, v);
    }
    let headers = Headers::new();
    headers.set("Accept", "application/json")?;
    headers.set("Content-Type", "application/x-www-form-urlencoded")?;
    headers.set("User-Agent", "gition")?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(headers).with_body(Some(form.query().unwrap_or_default().into()));
    let req = Request::new_with_init("https://github.com/login/oauth/access_token", &init)?;
    let v: Value = Fetch::Request(req).send().await?.json().await?;
    if let Some(err) = v["error"].as_str() {
        return Err(Error::RustError(format!("GitHub token exchange failed: {err}")));
    }
    Ok(v)
}

fn session_from(v: &Value) -> Result<Session> {
    let at = now();
    let expiry = |field: &str| v[field].as_f64().map(|secs| at + secs * 1000.0);
    Ok(Session {
        access: v["access_token"].as_str().ok_or_else(|| Error::RustError("no access token".into()))?.into(),
        access_exp: expiry("expires_in"),
        refresh: v["refresh_token"].as_str().map(str::to_string),
        refresh_exp: expiry("refresh_token_expires_in"),
    })
}

fn cipher(env: &Env) -> Result<Aes256Gcm> {
    let key = Sha256::digest(env.secret("SESSION_KEY")?.to_string().as_bytes());
    Aes256Gcm::new_from_slice(&key).map_err(|e| Error::RustError(e.to_string()))
}

fn session_cookie(env: &Env, s: &Session) -> Result<String> {
    let mut nonce = [0u8; 12];
    getrandom::getrandom(&mut nonce).map_err(|e| Error::RustError(e.to_string()))?;
    let plain = serde_json::to_vec(s)?;
    let mut sealed = nonce.to_vec();
    sealed.extend(
        cipher(env)?
            .encrypt(Nonce::from_slice(&nonce), plain.as_slice())
            .map_err(|e| Error::RustError(e.to_string()))?,
    );
    // Lasts as long as the refresh token, or 30 days for tokens that don't expire.
    let max_age =
        s.refresh_exp.or(s.access_exp).map(|ms| ((ms - now()) / 1000.0).max(0.0) as u64).unwrap_or(30 * 86400);
    Ok(cookie(SESSION, &B64.encode(sealed), max_age))
}

fn open(env: &Env, value: &str) -> Result<Session> {
    let bad = || Error::RustError("bad session".into());
    let sealed = B64.decode(value).map_err(|_| bad())?;
    if sealed.len() < 12 {
        return Err(bad());
    }
    let (nonce, data) = sealed.split_at(12);
    let plain = cipher(env)?.decrypt(Nonce::from_slice(nonce), data).map_err(|_| bad())?;
    Ok(serde_json::from_slice(&plain)?)
}
