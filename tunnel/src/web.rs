use anyhow::{Context, Result, bail};
use axum::extract::{Query, Request};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Form, Json};
use axum_extra::headers;
use cookie::Cookie;
use generic_array::GenericArray;
use http::Method;
use oauth2::{StandardDeviceAuthorizationResponse, TokenResponse};
use rand::{Rng, TryRngCore};
use serde::{Deserialize, Serialize};
use tower_cookies::{CookieManagerLayer, Cookies};

use crate::common::Environment;
use crate::db::User;
use crate::entity::{auth_tokens, device_tokens, sessions};

#[derive(Clone)]
struct Storage {
    db: crate::db::DB,
}

pub fn make_random_hex_string() -> String {
    let mut key = [0u8; 16];
    rand::rng().try_fill_bytes(&mut key).unwrap();
    let array = GenericArray::from_array(key);
    let session_id = format!("{:x}", array);
    session_id
}

fn make_random_challenge_string() -> String {
    const CHARSET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut rng = rand::rng();
    let one_char = || CHARSET[rng.random_range(0..CHARSET.len())] as char;
    let mut first: String = std::iter::repeat_with(one_char).take(8).collect();
    let second: String = first.split_off(4);
    format!("{}-{}", first, second)
}

impl Storage {
    fn new(db: crate::db::DB) -> Self {
        Self { db: db }
    }

    async fn get_user_for_session(&self, session: &str) -> Result<Option<(sessions::Model, User)>> {
        // TODO: transaction?
        let session = match self.db.get_session_token_by_id(session).await? {
            None => return Ok(None),
            Some(session) => session,
        };
        let user = self.db.get_user_by_id(&session.user_id).await?.unwrap();

        Ok(Some((session, user.clone())))
    }

    async fn create_device_token(&self) -> Result<device_tokens::Model> {
        let id = make_random_hex_string();
        let challenge = make_random_challenge_string();

        let now = chrono::Utc::now();
        let interval = chrono::Duration::seconds(5);

        let token = device_tokens::Model {
            device_code: id.clone(),
            user_code: challenge,
            state: device_tokens::TokenState::Pending,
            approved_by_user_id: "".into(),
            next_poll_at: now + interval,
            interval_seconds: interval.num_seconds(),
            expires_at: now + chrono::Duration::minutes(15),
        };

        self.db.insert_device_token(&token).await?;

        Ok(token)
    }

    async fn get_device_token(&self, challenge_string: &String) -> Result<device_tokens::Model> {
        self.db
            .get_device_token_by_user_code(challenge_string)
            .await?
            .context("missing token")
    }

    async fn approve_device_token(&self, device_code: &String, user_id: &String) -> Result<()> {
        // TODO: tx

        let mut token = self
            .db
            .get_device_token_by_device_code(device_code)
            .await?
            .context("missing token")?;

        if token.state != device_tokens::TokenState::Pending {
            bail!("not pending");
        }

        token.state = device_tokens::TokenState::Approved;
        token.approved_by_user_id = user_id.clone();

        self.db.update_device_token(&token).await?;

        // TODO: name token? wait for it to show up?

        Ok(())
    }

    async fn reject_device_token(&self, device_code: &String) -> Result<()> {
        // TODO: tx

        let mut token = self
            .db
            .get_device_token_by_device_code(device_code)
            .await?
            .context("missing token")?;

        if token.state != device_tokens::TokenState::Pending {
            bail!("not pending");
        }

        token.state = device_tokens::TokenState::Rejected;

        self.db.update_device_token(&token).await?;

        Ok(())
    }

    async fn poll_device_token(
        &self,
        device_code: &String,
    ) -> Result<(Option<device_tokens::Model>, Option<auth_tokens::Model>)> {
        // TODO: tx

        // TODO: update polling info?

        let mut token = match self.db.get_device_token_by_device_code(device_code).await? {
            Some(token) => token,
            None => return Ok((None, None)),
        };

        if token.state != device_tokens::TokenState::Approved {
            return Ok((Some(token), None));
        }

        token.state = device_tokens::TokenState::Exchanged;
        self.db.update_device_token(&token).await?;

        let auth_token = self
            .db
            .create_auth_token(&token.approved_by_user_id)
            .await?;

        return Ok((None, Some(auth_token)));
    }

    async fn get_auth_token(&self, auth_token_id: &str) -> Result<(auth_tokens::Model, User)> {
        // TODO: transaction

        let token = self
            .db
            .get_auth_token_by_id(auth_token_id)
            .await?
            .context("missing token")?;

        let user = self
            .db
            .get_user_by_id(&token.user_id)
            .await?
            .context("missing user")?;

        Ok((token, user))
    }
}

type Oauth2Client = oauth2::basic::BasicClient<
    oauth2::EndpointSet,
    oauth2::EndpointNotSet,
    oauth2::EndpointNotSet,
    oauth2::EndpointNotSet,
    oauth2::EndpointSet,
>;

#[derive(Clone)]
struct ServerState {
    env: Environment,
    storage: Storage,
    oauth2: Oauth2Client,
    available_suffixes: Vec<String>,
    web_base_url: String,
}

#[derive(Clone)]
struct UserInfo {
    user: User,
    session: sessions::Model,
}

// based on https://pkg.go.dev/net/http@master#CrossOriginProtection
fn check_cross_origin(req: &Request) -> Result<(), Response> {
    match req.method() {
        // safe methods are always allowed
        &Method::GET | &Method::HEAD | &Method::POST => {
            return Ok(());
        }
        _ => (),
    };

    if let Some(value) = req.headers().get("Sec-Fetch-Site") {
        return match value.as_bytes() {
            // same-origin is ok
            b"same-origin" | b"none" => Ok(()),

            // anything else is not ok
            _ => Err((
                axum::http::StatusCode::FORBIDDEN,
                "forbidden cross-origin request",
            )
                .into_response()),
        };
    }

    return match req.headers().get("Origin") {
        // got origin, but not sec-fetch-site
        Some(_) => Err((
            axum::http::StatusCode::FORBIDDEN,
            "please update your browser: got origin header but not sec-fetch-site",
        )
            .into_response()),

        // assume not browser request. not a csrf violation
        None => Ok(()),
    };
}

async fn cross_origin_check_middleware(
    req: Request,
    next: axum::middleware::Next,
) -> Result<Response, Response> {
    check_cross_origin(&req)?;
    Ok(next.run(req).await.into_response())
}

#[axum::debug_middleware]
async fn auth_middleware(
    state: Extension<ServerState>,
    cookies: Cookies,
    mut req: Request,
    next: axum::middleware::Next,
) -> Result<Response, Response> {
    /*
        let auth_header = req
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|header| header.to_str().ok());

    let auth_header = if let Some(auth_header) = auth_header {
            auth_header
        } else {
            return Err(axum::http::StatusCode::UNAUTHORIZED);
        };
        */

    let mut user_info: Option<UserInfo> = None;

    // let mut cookies = CookieJar::from_headers(req.headers());

    // __Host- ?
    if let Some(session_id_cookie) = cookies.get("session_id") {
        if let Some((session, user)) = state
            .storage
            .get_user_for_session(session_id_cookie.value())
            .await
            .unwrap()
        {
            user_info = Some(UserInfo {
                user: user,
                session: session,
            })
        } else {
            remove_cookie(&cookies, "session_id".into());
        }
    }

    req.extensions_mut().insert(user_info);

    // if let Some(current_user) = authorize_current_user(auth_header).await {
    // insert the current user into a request extension so the handler can
    // extract it
    Ok(next.run(req).await.into_response())
    // } else {
    // Err(axum::http::StatusCode::UNAUTHORIZED)
    // }
}

async fn index_handler(Extension(user): Extension<Option<UserInfo>>) -> Response {
    let tmpl = maud::html!(
        (maud::DOCTYPE)
        html {
            head {
                title {
                    "hello"
                }
            }
            body {
                @if let Some(user_info) = user {
                    p {
                        "hello " (user_info.user.email)
                    }

                    p {
                        @if let Some(id) = user_info.user.github_user_id {
                            "your github user id " (id)
                        } @else {
                            "your have no github user id"
                        }
                    }

                    p {
                        "your domains are: "
                        @for domain in user_info.user.domains {
                            (domain) " "
                        }
                    }

                    // TODO: DNS records?
                    // TODO: auth tokens? (last use, id, ip, ...?)
                    // TODO: log?

                    form method="post" action="/logout" {
                        "would you like to "
                        input type="submit" value="log out?" {}
                    }
                } @else {
                    "would you like to "
                    a href="/login?redirect_to=%2F" { "login?" }
                }
            }
        }
    );

    axum::response::Html(tmpl.into_string()).into_response()
}

#[derive(Serialize)]
struct Oauth2DeviceAuthorizationResponse {
    device_code: String,      // device code to poll with
    user_code: String,        // user code to type
    verification_uri: String, // uri for user to go to
    expires_in: i64,          // expiration, in seconds
    interval: i64,            // poll interval
}

async fn oauth2_device_code_handler(Extension(state): Extension<ServerState>) -> impl IntoResponse {
    let token = state.storage.create_device_token().await.unwrap();

    let now = chrono::Utc::now();

    let response = Oauth2DeviceAuthorizationResponse {
        device_code: token.device_code,
        user_code: token.user_code,
        verification_uri: format!("{}/login/device", state.web_base_url),
        expires_in: (token.expires_at - now).num_seconds(),
        interval: token.interval_seconds,
    };

    axum::response::Json(response)
}

#[derive(Deserialize)]
struct TokenRequest {
    // TODO: client_id, scope, ???
    device_code: String,
}

#[derive(Serialize)]
struct Oauth2DeviceTokenResponse {
    access_token: String,          // device code to poll with
    token_type: String,            // eg. "bearer"
    expires_in: i32,               // expiration time in seconds...
    refresh_token: Option<String>, // optional refresh token. why???
    scope: Option<String>,         // ehh?
}

#[derive(Serialize)]
struct Oauth2ErrorResponse {
    error: String,
}

async fn oauth2_device_token_handler(
    Extension(state): Extension<ServerState>,
    form: Form<TokenRequest>,
) -> Response {
    let (device_token, auth_token) = state
        .storage
        .poll_device_token(&form.device_code)
        .await
        .unwrap();

    if let Some(auth_token) = auth_token {
        let response = Oauth2DeviceTokenResponse {
            access_token: auth_token.id,
            token_type: "bearer".into(),
            expires_in: 60 * 60 * 24 * 365 * 10,
            refresh_token: None,
            scope: None,
        };
        return axum::response::Json(response).into_response();
    }

    let mut error = "bad_request".into();

    if let Some(device_token) = device_token {
        if device_token.state == device_tokens::TokenState::Pending {
            error = "authorization_pending".into();
        } else if device_token.state == device_tokens::TokenState::Rejected {
            error = "access_denied".into();
        }
        // TODO: for already exchanged tokens, return invalid_grant also?
        //
        // TODO: check "slow_down";
        // TODO: check "expired_token"
        // huh? invalid state?
    } else {
        error = "invalid_grant".into();
    }

    let response = Oauth2ErrorResponse { error: error };

    (
        axum::http::StatusCode::BAD_REQUEST,
        axum::response::Json(response),
    )
        .into_response()
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ApiTestRequest {}

#[derive(Serialize, Deserialize, Debug)]
pub struct ApiTestResponse {
    pub email: String,
    pub domains: Vec<String>,
    pub available_suffixes: Vec<String>,
}

#[axum::debug_handler]
async fn api_test_handler(
    Extension(state): Extension<ServerState>,
    axum_extra::extract::TypedHeader(auth): axum_extra::extract::TypedHeader<
        headers::Authorization<headers::authorization::Bearer>,
    >,
) -> impl IntoResponse {
    let (_auth_token, user) = state
        .storage
        .get_auth_token(auth.token())
        .await
        .expect("bad token");

    let response = ApiTestResponse {
        email: user.email,
        domains: user.domains,
        available_suffixes: state.available_suffixes.clone(),
    };

    axum::response::Json(response)
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ApiRegisterDomainRequest {
    pub domain: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ApiRegisterDomainResponse {}

#[axum::debug_handler]
async fn api_register_domain_handler(
    Extension(state): Extension<ServerState>,
    axum_extra::extract::TypedHeader(auth): axum_extra::extract::TypedHeader<
        headers::Authorization<headers::authorization::Bearer>,
    >,
    req: Json<ApiRegisterDomainRequest>,
) -> impl IntoResponse {
    let (_auth_token, user) = state
        .storage
        .get_auth_token(auth.token())
        .await
        .expect("bad token");

    let ok = state
        .available_suffixes
        .iter()
        .any(|suffix| req.domain.ends_with(suffix));
    if !ok {
        panic!("bad ending");
    }

    // TODO: check allowed? limit number of domains? available suffix?
    state
        .storage
        .db
        .register_user_domain(&user.id, &req.domain)
        .await
        .expect("failed to register domain");

    let response = ApiRegisterDomainResponse {};

    axum::response::Json(response)
}

async fn get_login_device_handler(Extension(user): Extension<Option<UserInfo>>) -> Response {
    if user.is_none() {
        return axum::response::Redirect::to("/login?redirect_to=%2Flogin%2Fdevice")
            .into_response();
    }

    let tmpl = maud::html!(
        (maud::DOCTYPE)
        html {
            head {
                title {
                    "hello"
                }
            }
            body {
                form method="post" {
                    label for="user_code" { "Code" }
                    input id="user_code" name="user_code" type="text" { }
                    input type="submit" { "Submit" }
                }
                "type the code"
            }
        }
    );

    axum::response::Html(tmpl.into_string()).into_response()
}

#[derive(Deserialize)]
struct LoginDeviceForm {
    user_code: String,
}

async fn post_login_device_handler(
    Extension(state): Extension<ServerState>,
    Extension(user_info): Extension<Option<UserInfo>>,
    form: Form<LoginDeviceForm>,
) -> Response {
    let token = state
        .storage
        .get_device_token(&form.user_code)
        .await
        .expect("where is my token");

    if token.state != device_tokens::TokenState::Pending {
        panic!("bad");
    }

    let name = user_info.map_or("unknown???".into(), |u| u.user.email);

    let tmpl = maud::html!(
        (maud::DOCTYPE)
        html {
            head {
                title {
                    "hello"
                }
            }
            body {
                p {
                    "you must be"
                    ( name )
                }
                p {
                    "found your token"
                    ( token.user_code )
                    ( token.device_code )
                }
                form action="/login/device/reject" method="post" {
                    input type="hidden" name="user_code" value=( token.user_code ) {}
                    input type="submit" value="Cancel" {}
                }
                form action="/login/device/authorize" method="post" {
                    input type="hidden" name="user_code" value=( token.user_code ) {}
                    input type="submit" value="Approve" {}
                }
            }
        }
    );

    axum::response::Html(tmpl.into_string()).into_response()
}

async fn post_approve_device_handler(
    Extension(state): Extension<ServerState>,
    Extension(user): Extension<Option<UserInfo>>,
    form: Form<LoginDeviceForm>,
) -> Response {
    let token = state
        .storage
        .get_device_token(&form.user_code)
        .await
        .expect("where is my token");

    state
        .storage
        .approve_device_token(
            &token.device_code,
            &user.expect("must be logged in").user.id,
        )
        .await
        .expect("approving failed");

    let tmpl = maud::html!(
        (maud::DOCTYPE)
        html {
            head {
                title {
                    "hello"
                }
            }
            body {
                p {
                    "DONE"
                }
                a href="/" { "home" }
            }
        }
    );

    axum::response::Html(tmpl.into_string()).into_response()
}

async fn post_reject_device_handler(
    Extension(state): Extension<ServerState>,
    form: Form<LoginDeviceForm>,
) -> Response {
    let token = state
        .storage
        .get_device_token(&form.user_code)
        .await
        .expect("where is my token");

    state
        .storage
        .reject_device_token(&token.device_code)
        .await
        .expect("rejecting failed");

    let tmpl = maud::html!(
        (maud::DOCTYPE)
        html {
            head {
                title {
                    "hello"
                }
            }
            body {
                p {
                    "DONE"
                }
                a href="/" { "home" }
            }
        }
    );

    axum::response::Html(tmpl.into_string()).into_response()
}

async fn login_handler(query: axum::extract::Query<LoginQuery>) -> Response {
    let tmpl = maud::html!(
        (maud::DOCTYPE)
        html {
            head {
                title {
                    "hello"
                }
            }
            body {
                "time to login"
                a href=(format!("/login/github?redirect_to={}", urlencoding::encode(&query.redirect_to))) { "with github" }
            }
        }
    );

    axum::response::Html(tmpl.into_string()).into_response()
}

async fn logout_handler(
    cookies: Cookies,
    Extension(user): Extension<Option<UserInfo>>,
    Extension(state): Extension<ServerState>,
) -> impl IntoResponse {
    if let Some(user) = user {
        state
            .storage
            .db
            .delete_session_token(&user.session.id)
            .await
            .expect("huh");
    }
    cookies.remove("session_id".into());

    axum::response::Redirect::to("/").into_response()
}

#[derive(Serialize, Deserialize)]
struct GithubOauth2State {
    token: String,
    redirect_to: String,
}

#[derive(Deserialize)]
struct LoginQuery {
    redirect_to: String,
}

async fn login_github_handler(
    cookies: Cookies,
    query: axum::extract::Query<LoginQuery>,
    Extension(state): Extension<ServerState>,
    // Host(host): Host,
) -> impl IntoResponse {
    let (url, oauth2_csrf_token) = state
        .oauth2
        .authorize_url(oauth2::CsrfToken::new_random)
        .add_scope(oauth2::Scope::new("user:email".to_string()))
        .url();

    let state = GithubOauth2State {
        token: oauth2_csrf_token.into_secret(),
        redirect_to: query.redirect_to.clone(),
    };
    let encoded_state = serde_urlencoded::to_string(state).expect("huh");
    set_cookie(
        &cookies,
        "github_oauth2_state".into(),
        encoded_state,
        Some(cookie::time::Duration::minutes(30)),
    );
    axum::response::Redirect::to(&url.to_string())
}

#[derive(Deserialize)]
struct LoginCallbackQuery {
    code: String,
    state: String,
}

#[derive(Deserialize)]
struct GithubUserInfo {
    login: String, // user name
    id: i64,       // stable id
    email: String, // primary email
}

fn set_cookie(
    cookies: &Cookies,
    name: String,
    value: String,
    expiration: Option<cookie::time::Duration>,
) {
    // TODO: "__Host-" prefix (if not on localhost)
    let mut cookie = Cookie::build((name, value))
        .path("/")
        // .secure(true) // safari doesn't like on localhost?
        .http_only(true)
        .same_site(cookie::SameSite::Lax);
    if let Some(expiration) = expiration {
        cookie = cookie.max_age(expiration);
    }
    cookies.add(cookie.build());
}

fn remove_cookie(cookies: &Cookies, name: String) {
    // TODO: "__Host-" prefix (if not on localhost)
    let cookie = Cookie::build((name, ""))
        .path("/")
        // .secure(true) // safari doesn't like on localhost?
        .http_only(true)
        .same_site(cookie::SameSite::Lax);
    cookies.remove(cookie.build());
}
async fn login_callback_handler(
    cookies: Cookies,
    Extension(state): Extension<ServerState>,
    query: Query<LoginCallbackQuery>,
    // Host(host): Host,
) -> Response {
    let github_oauth2_state_cookie = cookies.get("github_oauth2_state").expect("need state");
    let github_oauth2_state: GithubOauth2State =
        serde_urlencoded::from_str(github_oauth2_state_cookie.value()).expect("bad encoding");
    remove_cookie(&cookies, "github_oauth2_state".into());

    if query.state != github_oauth2_state.token {
        // TODO: constant time compare?
        panic!("bad state");
    }

    let http_client = state.env.external_reqwest_client;

    let token_result = state
        .oauth2
        .exchange_code(oauth2::AuthorizationCode::new(query.code.clone()))
        .request_async(&http_client)
        .await
        .expect("request should be good...");
    // .unwrap("token should be good");

    let token = token_result.access_token();

    let user: GithubUserInfo = http_client
        // let text = http_client
        .get("https://api.github.com/user")
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token.secret())
        .header("User-Agent", "github.com/jellevandenhooff/pos2/tunnel")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .expect("get user")
        .json()
        .await
        .expect("unmarshal json");
    // TODO: if bad json, somehow try to log it?

    // TODO: create account, maybe
    let maybe_user = state
        .storage
        .db
        .get_user_by_github_user_id(user.id)
        .await
        .expect("lookup user");

    let user = match maybe_user {
        Some(user) => user,
        None => {
            state
                .storage
                .db
                .create_user(&user.email, Some(user.id), vec![])
                .await
                .expect("got user") // TODO: handle failures?
        }
    };

    let session_token = state
        .storage
        .db
        .create_session_token(&user.id)
        .await
        .expect("creating token");

    set_cookie(
        &cookies,
        "session_id".into(),
        session_token.id,
        Some(cookie::time::Duration::hours(24)),
    );

    axum::response::Redirect::to(&github_oauth2_state.redirect_to).into_response()
}

fn make_github_oauth2_client(
    web_base_url: String,
    client_id: String,
    client_secret: String,
) -> Result<Oauth2Client> {
    let client = oauth2::basic::BasicClient::new(oauth2::ClientId::new(client_id))
        .set_client_secret(oauth2::ClientSecret::new(client_secret))
        .set_auth_uri(oauth2::AuthUrl::new(
            "https://github.com/login/oauth/authorize".to_string(),
        )?)
        .set_token_uri(oauth2::TokenUrl::new(
            "https://github.com/login/oauth/access_token".to_string(),
        )?)
        // Set the URL the user will be redirected to after the authorization process.
        .set_redirect_uri(oauth2::RedirectUrl::new(format!(
            "{}/login/callback",
            web_base_url
        ))?);
    Ok(client)
}

pub async fn make_router(
    env: Environment,
    db: crate::db::DB,
    available_suffixes: Vec<String>,
    web_base_url: String,
    github_oauth2_client_id: String,
    github_oauth2_client_secret: String,
) -> Result<axum::Router> {
    let app = axum::Router::new()
        .route("/", get(index_handler))
        .route("/login/device/code", post(oauth2_device_code_handler))
        .route(
            "/login/oauth/access_token",
            post(oauth2_device_token_handler),
        )
        .route("/login/device", get(get_login_device_handler))
        .route("/login/device", post(post_login_device_handler))
        .route("/login/device/authorize", post(post_approve_device_handler))
        .route("/login/device/reject", post(post_reject_device_handler))
        .route("/login", get(login_handler))
        .route("/logout", post(logout_handler))
        .route("/login/github", get(login_github_handler))
        .route("/login/callback", get(login_callback_handler))
        .route("/api/test", get(api_test_handler))
        .route("/api/domain/register", get(api_register_domain_handler))
        .layer(axum::middleware::from_fn(auth_middleware))
        .layer(axum::middleware::from_fn(cross_origin_check_middleware))
        .layer(Extension(ServerState {
            env: env,
            storage: Storage::new(db),
            oauth2: make_github_oauth2_client(
                web_base_url.clone(),
                github_oauth2_client_id,
                github_oauth2_client_secret,
            )?,
            available_suffixes: available_suffixes,
            web_base_url: web_base_url,
        }))
        .layer(CookieManagerLayer::new());

    Ok(app)
}

pub struct ApiClient {
    http_client: reqwest::Client,
    host: String,
    key: String,
}

impl ApiClient {
    pub fn new(env: &Environment, host: &str, key: &str) -> Self {
        Self {
            http_client: env.reqwest_client.clone(),
            host: host.into(),
            key: key.into(),
        }
    }

    async fn roundtrip<Resp>(&self, endpoint: &str, req: &impl Serialize) -> Result<Resp>
    where
        Resp: serde::de::DeserializeOwned,
    {
        // TODO: wrap errors with requested endpoint/host?
        // TODO: complain about error response codes?
        // TODO: log bad bodies/helpful error messages?
        let resp = self
            .http_client
            .get(format!("{}{endpoint}", &self.host))
            .bearer_auth(&self.key)
            .json(req)
            .send()
            .await?
            .json()
            .await?;
        Ok(resp)
    }

    pub async fn test_request(&self, req: &ApiTestRequest) -> Result<ApiTestResponse> {
        self.roundtrip("/api/test", req).await
    }

    pub async fn register_domain(
        &self,
        req: &ApiRegisterDomainRequest,
    ) -> Result<ApiRegisterDomainResponse> {
        self.roundtrip("/api/domain/register", req).await
    }
}

pub async fn run_device(env: &Environment, endpoint: &str) -> Result<String> {
    // let client_id = env::var("GITHUB_OAUTH2_CLIENT_ID")?;
    // let client_secret = env::var("GITHUB_OAUTH2_CLIENT_SECRET")?;
    let client_id = "ok".into();
    let client_secret = "ok".into();

    let client = oauth2::basic::BasicClient::new(oauth2::ClientId::new(client_id))
        .set_client_secret(oauth2::ClientSecret::new(client_secret))
        .set_device_authorization_url(oauth2::DeviceAuthorizationUrl::new(format!(
            "{endpoint}/login/device/code"
        ))?)
        .set_token_uri(oauth2::TokenUrl::new(format!(
            "{endpoint}/login/oauth/access_token"
        ))?);

    let http_client = &env.reqwest_client;

    let details: StandardDeviceAuthorizationResponse = client
        .exchange_device_code()
        .request_async(http_client)
        .await?;

    println!(
        "Open this URL in your browser:\n{}\nand enter the code: {}",
        details.verification_uri().to_string(),
        details.user_code().secret().to_string()
    );

    let token_result = client
        .exchange_device_access_token(&details)
        .request_async(http_client, tokio::time::sleep, None)
        .await?;

    println!("Got token: {}", token_result.access_token().secret());

    // TODO: skip this test step?
    let client = ApiClient::new(env, endpoint, token_result.access_token().secret());
    let _resp = client.test_request(&ApiTestRequest {}).await?;
    // println!("Got response: {:?}", resp);

    Ok(token_result.access_token().secret().clone())
}
