//! Configuration page on `http://PICO_IP/`, reachable from the host over the
//! USB network. Request parsing, validation and the page live in
//! `deskflow_proto::{web, settings, auth}`; this is the socket glue.

use core::fmt::Write as _;

use deskflow_proto::auth::{self, Access, PasswordChange, PasswordForm};
use deskflow_proto::diag::{self, Diagnostics, Room};
use deskflow_proto::pairing::Command;
use deskflow_proto::panel::{self, Press};
use deskflow_proto::settings::Settings;
use deskflow_proto::web::{self, JsonStr, Method, Request, Status};
use embassy_net::Stack;
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use embedded_io_async::Write;

use crate::storage::{self, SharedFlash, SharedStorage};
#[cfg(feature = "pico2-w")]
use crate::update;
use crate::{ble, board, clipboard, config, password, status, usb};

const PORT: u16 = 80;
/// Listening sockets; a browser opens a couple of connections at once.
pub const SERVERS: usize = 2;
const TIMEOUT: Duration = Duration::from_secs(5);
/// A whole request, which a firmware upload makes long.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
/// Hashing a password takes a while on the slower board; keep the socket
/// for that.
const AUTH_TIMEOUT: Duration = Duration::from_secs(15);
/// Request head plus body.
const REQUEST_MAX: usize = 2048;
const BODY_MAX: usize = 768;
const LOG_CHUNK: usize = 1024;
/// Pause after a wrong password, against guessing.
const WRONG_PASSWORD_DELAY: Duration = Duration::from_secs(1);

pub struct Context {
    pub settings: &'static Settings,
    pub storage: &'static SharedStorage,
    #[cfg_attr(not(feature = "pico2-w"), allow(dead_code))]
    pub flash: &'static SharedFlash,
}

#[derive(Debug)]
struct Disconnected;

type Result<T> = core::result::Result<T, Disconnected>;

#[embassy_executor::task(pool_size = SERVERS)]
pub async fn server_task(stack: Stack<'static>, ctx: &'static Context) -> ! {
    let mut rx = [0u8; 1024];
    let mut tx = [0u8; 2048];
    let mut req = [0u8; REQUEST_MAX];
    loop {
        let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
        socket.set_timeout(Some(AUTH_TIMEOUT));
        if socket.accept(PORT).await.is_err() {
            continue;
        }
        let restart = matches!(with_timeout(REQUEST_TIMEOUT, handle(&mut socket, &mut req, ctx)).await, Ok(Ok(true)));
        socket.close();
        let _ = with_timeout(TIMEOUT, socket.flush()).await;
        if restart {
            usb::restart().await;
        }
        socket.abort();
    }
}

/// Serves one request. Returns whether the device should restart.
async fn handle(socket: &mut TcpSocket<'_>, buf: &mut [u8; REQUEST_MAX], ctx: &Context) -> Result<bool> {
    let mut len = 0;
    let head = loop {
        let n = with_timeout(TIMEOUT, socket.read(&mut buf[len..])).await.map_err(|_| Disconnected)?.map_err(|_| Disconnected)?;
        if n == 0 {
            return Err(Disconnected);
        }
        len += n;
        match web::parse_head(&buf[..len]) {
            Ok(Some(head)) => break head,
            Ok(None) if len < buf.len() => continue,
            Ok(None) => return respond_text(socket, 413, "request too large").await.map(|()| false),
            Err(_) => return respond_text(socket, 400, "bad request").await.map(|()| false),
        }
    };
    // `head` borrows `buf`; copy out what's needed before reading the body.
    let Request { method, path, query, host, content_length, csrf_header, cookie, head_len } = head;

    if !web::host_is(host, config::PICO_IP.octets()) {
        return respond_text(socket, 403, "use the device's IP address").await.map(|()| false);
    }

    let mut path_buf = heapless::String::<32>::new();
    let _ = path_buf.push_str(path);
    let path = path_buf.as_str();
    let since = web::query_u32(query, "since").unwrap_or(0);
    let since_index = web::query_u32(query, "index").unwrap_or(u32::MAX);
    let button = web::query_u32(query, "button").unwrap_or(u32::MAX);
    let long_press = web::query_u32(query, "long") == Some(1);
    let password = password::current();
    let logged_in = match (&password, auth::cookie_token(cookie)) {
        (Some(p), Some(token)) => p.token_valid(token),
        _ => false,
    };

    if method == Method::Post && !csrf_header {
        return respond_text(socket, 403, "missing X-Bridge header").await.map(|()| false);
    }
    if auth::access(method, path, password.as_ref(), logged_in) == Access::LoginRequired {
        respond(socket, 401, "application/json", br#"{"error":"log in first","login":true}"#, "").await?;
        return Ok(false);
    }

    // Text for typing goes straight into the clipboard buffer; it can be far
    // larger than `buf`.
    if (method, path) == (Method::Post, "/api/clipboard") {
        if content_length > board::CLIPBOARD_MAX {
            return respond_text(socket, 413, "too much text").await.map(|()| false);
        }
        let first = (len - head_len).min(content_length);
        let mut chunk = [0u8; 512];
        chunk[..first].copy_from_slice(&buf[head_len..head_len + first]);
        return receive_clipboard(socket, &mut chunk, first, content_length).await.map(|()| false);
    }

    #[cfg(feature = "pico2-w")]
    if (method, path) == (Method::Post, "/api/update") {
        return receive_update(socket, buf, len.min(head_len + content_length), head_len, content_length, ctx).await;
    }

    let body_len = match method {
        Method::Post => {
            if content_length > BODY_MAX || head_len + content_length > buf.len() {
                return respond_text(socket, 413, "form too large").await.map(|()| false);
            }
            while len < head_len + content_length {
                let n = socket.read(&mut buf[len..]).await.map_err(|_| Disconnected)?;
                if n == 0 {
                    return Err(Disconnected);
                }
                len += n;
            }
            content_length
        }
        _ => 0,
    };
    let body = &buf[head_len..head_len + body_len];
    status::note_web_served();

    match (method, path) {
        (Method::Get, "/") => {
            respond(socket, 200, "text/html; charset=utf-8", web::PAGE.as_bytes(), "").await?;
            Ok(false)
        }
        (Method::Get, "/api/state") => {
            let mut body = heapless::String::<2048>::new();
            let _ = state_json(&mut body, ctx, logged_in);
            respond(socket, 200, "application/json", body.as_bytes(), "").await?;
            Ok(false)
        }
        (Method::Get, "/api/log") => {
            let mut chunk = [0u8; LOG_CHUNK];
            let (n, next) = crate::logging::read(since, &mut chunk);
            let mut extra = heapless::String::<32>::new();
            let _ = write!(extra, "X-Log-Next: {}\r\n", next);
            respond(socket, 200, "text/plain; charset=utf-8", &chunk[..n], &extra).await?;
            Ok(false)
        }
        (Method::Post, "/api/restart") => {
            respond(socket, 200, "application/json", br#"{"ok":true}"#, "").await?;
            Ok(true)
        }
        (Method::Post, "/api/forget-laptop") => {
            if storage::forget_bonds(&mut *ctx.storage.lock().await).await.is_err() {
                log::error!("web: failed to forget the paired laptops");
                return respond_text(socket, 500, "couldn't write flash").await.map(|()| false);
            }
            log::info!("web: forgot the paired laptops");
            respond(socket, 200, "application/json", br#"{"ok":true}"#, "").await?;
            Ok(true)
        }
        (Method::Post, "/api/type-clipboard") => {
            clipboard::request_typing();
            respond(socket, 200, "application/json", br#"{"ok":true}"#, "").await?;
            Ok(false)
        }
        (Method::Post, "/api/stop-typing") => {
            clipboard::stop_typing();
            respond(socket, 200, "application/json", br#"{"ok":true}"#, "").await?;
            Ok(false)
        }
        (Method::Get, "/api/panel") => {
            let mut body = heapless::String::<128>::new();
            let _ = panel::write_json(&mut body, status::leds(), &ctx.settings.buttons);
            respond(socket, 200, "application/json", body.as_bytes(), "").await?;
            Ok(false)
        }
        (Method::Post, "/api/panel/press") => {
            let press = if long_press { Press::Long } else { Press::Short };
            let Ok(button) = u8::try_from(button) else {
                return respond_text(socket, 400, "no such button").await.map(|()| false);
            };
            if crate::panel::VIRTUAL_PRESSES.try_send((button, press)).is_err() {
                return respond_text(socket, 503, "busy, try again").await.map(|()| false);
            }
            respond(socket, 200, "application/json", br#"{"ok":true}"#, "").await?;
            Ok(false)
        }
        (Method::Post, "/api/laptop/pair") => laptop_command(socket, Command::Pair).await.map(|()| false),
        (Method::Post, "/api/laptop/switch" | "/api/laptop/forget") => {
            let Ok(index) = u8::try_from(since_index) else {
                return respond_text(socket, 400, "no such laptop").await.map(|()| false);
            };
            let command = if path == "/api/laptop/switch" { Command::Switch(index) } else { Command::Forget(index) };
            laptop_command(socket, command).await.map(|()| false)
        }
        (Method::Post, "/api/settings") => save_settings(socket, body, ctx).await,
        (Method::Post, "/api/login") => login(socket, body).await.map(|()| false),
        (Method::Post, "/api/logout") => {
            let mut cookie = heapless::String::<96>::new();
            let _ = write!(cookie, "Set-Cookie: {}=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict\r\n", auth::COOKIE);
            respond(socket, 200, "application/json", br#"{"ok":true}"#, &cookie).await.map(|()| false)
        }
        (Method::Post, "/api/password") => change_password(socket, body, ctx).await.map(|()| false),
        (
            _,
            "/" | "/api/state" | "/api/log" | "/api/settings" | "/api/restart" | "/api/forget-laptop"
            | "/api/type-clipboard" | "/api/stop-typing" | "/api/clipboard" | "/api/laptop/pair"
            | "/api/laptop/switch" | "/api/laptop/forget" | "/api/panel" | "/api/panel/press" | "/api/update"
            | "/api/login" | "/api/logout" | "/api/password",
        ) => respond_text(socket, 405, "method not allowed").await.map(|()| false),
        _ => respond_text(socket, 404, "not found").await.map(|()| false),
    }
}

/// Replaces the clipboard with `total` bytes of text, the first `first` of
/// them already in `chunk`.
/// Takes a `.uf2` from the page and restarts into it.
#[cfg(feature = "pico2-w")]
async fn receive_update(
    socket: &mut TcpSocket<'_>,
    buf: &mut [u8; REQUEST_MAX],
    filled: usize,
    head_len: usize,
    total: usize,
    ctx: &Context,
) -> Result<bool> {
    let mut aligned = update::state_buffer();
    let Some(mut upload) = update::Upload::start(ctx.flash, &mut aligned) else {
        return respond_error(socket, 503, "another update is running", None).await.map(|()| false);
    };
    log::info!("update: receiving {} KB", total / 1024);
    let mut left = total - (filled - head_len);
    let mut result = upload.feed(&buf[head_len..filled]).await;
    while left > 0 && result.is_ok() {
        let wanted = left.min(buf.len());
        let n = match socket.read(&mut buf[..wanted]).await {
            Ok(n) if n > 0 => n,
            _ => return Err(Disconnected),
        };
        left -= n;
        result = upload.feed(&buf[..n]).await;
    }
    if result.is_ok() {
        result = upload.finish().await;
    }
    match result {
        Ok(()) => {
            respond(socket, 200, "application/json", br#"{"ok":true}"#, "").await?;
            Ok(true)
        }
        Err(message) => {
            log::warn!("update: refused: {}", message);
            respond_error(socket, 400, message, None).await.map(|()| false)
        }
    }
}

async fn laptop_command(socket: &mut TcpSocket<'_>, command: Command) -> Result<()> {
    if ble::COMMANDS.try_send(command).is_err() {
        return respond_text(socket, 503, "busy, try again").await;
    }
    respond(socket, 200, "application/json", br#"{"ok":true}"#, "").await
}

async fn receive_clipboard(socket: &mut TcpSocket<'_>, chunk: &mut [u8; 512], first: usize, total: usize) -> Result<()> {
    clipboard::begin();
    clipboard::append(&chunk[..first]);
    let mut left = total - first;
    while left > 0 {
        let wanted = left.min(chunk.len());
        match socket.read(&mut chunk[..wanted]).await {
            Ok(n) if n > 0 => {
                clipboard::append(&chunk[..n]);
                left -= n;
            }
            _ => {
                // Half a text isn't worth typing.
                clipboard::begin();
                clipboard::finish();
                return Err(Disconnected);
            }
        }
    }
    clipboard::finish();
    respond(socket, 200, "application/json", br#"{"ok":true}"#, "").await
}

async fn save_settings(socket: &mut TcpSocket<'_>, form: &[u8], ctx: &Context) -> Result<bool> {
    let next = match ctx.settings.with_form(form) {
        Ok(next) => next,
        Err(e) => return respond_error(socket, 400, e.message, Some(e.field)).await.map(|()| false),
    };
    if storage::save_settings(&mut *ctx.storage.lock().await, &next).await.is_err() {
        log::error!("web: failed to save settings");
        return respond_error(socket, 500, "couldn't write flash", None).await.map(|()| false);
    }
    respond(socket, 200, "application/json", br#"{"ok":true}"#, "").await?;
    Ok(true)
}

async fn login(socket: &mut TcpSocket<'_>, form: &[u8]) -> Result<()> {
    let Some(p) = password::current() else {
        return respond(socket, 200, "application/json", br#"{"ok":true}"#, "").await;
    };
    let mut buf = [0u8; auth::PASSWORD_MAX];
    let typed = web::form_value(form, "password", &mut buf).flatten().unwrap_or("");
    if !password::check(&p, typed).await {
        log::warn!("web: wrong password");
        Timer::after(WRONG_PASSWORD_DELAY).await;
        return respond_error(socket, 403, "wrong password", Some("password")).await;
    }
    respond_with_session(socket, &p).await
}

async fn change_password(socket: &mut TcpSocket<'_>, form: &[u8], ctx: &Context) -> Result<()> {
    let form = match PasswordForm::parse(form) {
        Ok(form) => form,
        Err(e) => return respond_error(socket, 400, e.message, Some(e.field)).await,
    };
    let current = password::current();
    if let Some(p) = &current
        && !password::check(p, &form.current).await
    {
        Timer::after(WRONG_PASSWORD_DELAY).await;
        return respond_error(socket, 403, "wrong password", Some("current")).await;
    }
    let next = match (form.change, current) {
        (PasswordChange::Remove, _) => None,
        (PasswordChange::Set(new), _) => Some(password::create(&new, form.protect_view).await),
        (PasswordChange::Keep, Some(p)) => Some(auth::Password { protect_view: form.protect_view, ..p }),
        (PasswordChange::Keep, None) => return respond_error(socket, 400, "enter a password", Some("new")).await,
    };
    if password::set(ctx.storage, next).await.is_err() {
        log::error!("web: failed to save the password");
        return respond_error(socket, 500, "couldn't write flash", None).await;
    }
    match &next {
        Some(p) => {
            log::info!("web: password saved (viewing {})", if p.protect_view { "protected" } else { "open" });
            // A new password ends every session; keep this one going.
            respond_with_session(socket, p).await
        }
        None => {
            log::info!("web: password removed");
            respond(socket, 200, "application/json", br#"{"ok":true}"#, "").await
        }
    }
}

async fn respond_with_session(socket: &mut TcpSocket<'_>, p: &auth::Password) -> Result<()> {
    let mut cookie = heapless::String::<160>::new();
    let _ = write!(
        cookie,
        "Set-Cookie: {}={}; Path=/; HttpOnly; SameSite=Strict\r\n",
        auth::COOKIE,
        password::new_token(p)
    );
    respond(socket, 200, "application/json", br#"{"ok":true}"#, &cookie).await
}

fn state_json(w: &mut impl core::fmt::Write, ctx: &Context, logged_in: bool) -> core::fmt::Result {
    let (clipboard_chars, clipboard_truncated, typing) = clipboard::status();
    let status = Status {
        clipboard_chars,
        clipboard_truncated,
        clipboard_max: board::CLIPBOARD_MAX,
        firmware: env!("BUILD_ID"),
        updates: cfg!(feature = "pico2-w"),
        typing,
        tls_handshake_ms: status::tls_handshake_ms(),
        deskflow: status::deskflow(),
        ble_connected: status::ble_connected(),
        ble_bonded: status::ble_bonded(),
        server_fingerprint: status::server_fingerprint(),
        client_fingerprint: status::client_fingerprint(),
        uptime_s: Instant::now().as_secs(),
    };
    let diagnostics = Diagnostics {
        ble_rssi: status::ble_rssi(),
        ble_interval_us: status::ble_interval_us(),
        ble_connections: status::ble_connections(),
        deskflow_sessions: status::deskflow_sessions(),
        latency: status::latency(),
        tls_path: status::tls_path(),
        backlog: status::backlog(),
        in_chip: status::in_chip(),
        flow_timeouts: status::flow_timeouts(),
        heap: Room { free: crate::HEAP.free(), size: crate::HEAP_SIZE },
        stacks: [core0_stack(), core1_stack()],
    };
    let password = password::current();
    w.write_str(r#"{"settings":"#)?;
    ctx.settings.write_json(w)?;
    w.write_str(r#","status":"#)?;
    status.write_json(w)?;
    w.write_str(r#","diag":"#)?;
    diagnostics.write_json(w)?;
    w.write_str(r#","laptops":"#)?;
    status::laptops().write_json(w)?;
    write!(
        w,
        r#","auth":{{"password":{},"protect_view":{},"logged_in":{}}}}}"#,
        password.is_some(),
        password.is_some_and(|p| p.protect_view),
        logged_in
    )
}

/// Core 0's stack, painted by cortex-m-rt at reset. flip-link puts it at
/// the bottom of RAM: `_stack_end` up to `_stack_start`.
fn core0_stack() -> Room {
    unsafe extern "C" {
        static _stack_start: u32;
        static _stack_end: u32;
    }
    let start = &raw const _stack_end;
    let size = (&raw const _stack_start as usize) - (start as usize);
    // SAFETY: the stack's unused end is only written by deeper calls, and a
    // torn read only misjudges the high-water mark.
    let words = unsafe { core::slice::from_raw_parts(start, size / 4) };
    Room { free: diag::stack_unused(words, cortex_m_rt::STACK_PAINT_VALUE), size }
}

/// Core 1's stack starts out zeroed.
fn core1_stack() -> Room {
    let start = (&raw const crate::CORE1_STACK).cast::<u32>();
    // SAFETY: as above; the stack is 32-byte aligned.
    let words = unsafe { core::slice::from_raw_parts(start, crate::CORE1_STACK_SIZE / 4) };
    Room { free: diag::stack_unused(words, 0), size: crate::CORE1_STACK_SIZE }
}

async fn respond_error(socket: &mut TcpSocket<'_>, status: u16, message: &str, field: Option<&str>) -> Result<()> {
    let mut body = heapless::String::<256>::new();
    let _ = write!(body, r#"{{"error":{},"field":"#, JsonStr(message));
    let _ = match field {
        Some(f) => write!(body, "{}}}", JsonStr(f)),
        None => body.write_str("null}"),
    };
    respond(socket, status, "application/json", body.as_bytes(), "").await
}

async fn respond_text(socket: &mut TcpSocket<'_>, status: u16, text: &str) -> Result<()> {
    respond(socket, status, "text/plain; charset=utf-8", text.as_bytes(), "").await
}

async fn respond(socket: &mut TcpSocket<'_>, status: u16, content_type: &str, body: &[u8], extra: &str) -> Result<()> {
    let mut head = heapless::String::<320>::new();
    web::write_head(&mut head, status, content_type, body.len(), extra).map_err(|_| Disconnected)?;
    socket.write_all(head.as_bytes()).await.map_err(|_| Disconnected)?;
    socket.write_all(body).await.map_err(|_| Disconnected)
}
