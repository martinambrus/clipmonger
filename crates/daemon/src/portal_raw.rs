//! Raw `GlobalShortcuts` session calls.
//!
//! ashpd follows the final portal spec: shortcuts go in `BindShortcuts`.
//! Plasma 5.27's portal implements an earlier draft that registers only
//! the shortcuts passed in `CreateSession`'s `shortcuts` option (its
//! `BindShortcuts` registers nothing). These calls pass the shortcuts in
//! both; newer portals drop the unknown `CreateSession` option.

use std::collections::HashMap;

use ashpd::zbus;
use ashpd::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use futures_util::StreamExt;

const DESKTOP: &str = "org.freedesktop.portal.Desktop";
const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.GlobalShortcuts";
/// Session handle token (see [`create_session`]).
const SESSION_TOKEN: &str = "clipmonger";

/// One shortcut to register: `(id, description, preferred trigger)`.
pub(crate) type Spec<'a> = (&'a str, &'a str, &'a str);

/// The portal's `a(sa{sv})` shortcut list.
fn shortcut_list(specs: &[Spec<'_>]) -> Vec<(String, HashMap<String, Value<'static>>)> {
    specs
        .iter()
        .map(|(id, description, trigger)| {
            let mut info = HashMap::new();
            info.insert(
                "description".to_string(),
                Value::from(description.to_string()),
            );
            info.insert(
                "preferred_trigger".to_string(),
                Value::from(trigger.to_string()),
            );
            (id.to_string(), info)
        })
        .collect()
}

/// A request or session handle token, unique within this process.
fn token(kind: &str) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("clipmonger_{kind}_{}_{n}", std::process::id())
}

/// The request object path the portal will use for `token`.
fn request_path(conn: &zbus::Connection, token: &str) -> zbus::Result<String> {
    let sender = conn
        .unique_name()
        .ok_or_else(|| zbus::Error::Failure("no unique bus name".to_string()))?;
    let sender = sender.as_str().trim_start_matches(':').replace('.', "_");
    Ok(format!("{DESKTOP_PATH}/request/{sender}/{token}"))
}

/// Call `method` and wait for its `Response`; the results on success.
async fn request<B>(
    conn: &zbus::Connection,
    handle_token: &str,
    method: &str,
    body: &B,
) -> zbus::Result<HashMap<String, OwnedValue>>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let path = request_path(conn, handle_token)?;
    let request = zbus::Proxy::new(
        conn,
        DESKTOP,
        ObjectPath::try_from(path)?,
        "org.freedesktop.portal.Request",
    )
    .await?;
    // Subscribe before the call, or a fast response is missed.
    let mut responses = request.receive_signal("Response").await?;
    let portal = zbus::Proxy::new(conn, DESKTOP, DESKTOP_PATH, INTERFACE).await?;
    let _: OwnedObjectPath = portal.call(method, body).await?;
    let Some(message) = responses.next().await else {
        return Err(zbus::Error::Failure(format!("{method}: no response")));
    };
    let (code, results): (u32, HashMap<String, OwnedValue>) = message.body().deserialize()?;
    match code {
        0 => Ok(results),
        1 => Err(zbus::Error::Failure(format!(
            "{method}: cancelled by the user"
        ))),
        _ => Err(zbus::Error::Failure(format!("{method}: failed"))),
    }
}

/// Create a session that already carries `specs` (Plasma 5.27).
///
/// # Errors
///
/// A D-Bus failure, or a portal response other than success.
pub(crate) async fn create_session(
    conn: &zbus::Connection,
    specs: &[Spec<'_>],
) -> zbus::Result<OwnedObjectPath> {
    let handle = token("req");
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", Value::from(handle.clone()));
    // Constant: Plasma 5.27 names the shortcut group after it, so a stable
    // token keeps the user's key choices across restarts. The previous
    // session is closed before a new one is created.
    options.insert("session_handle_token", Value::from(SESSION_TOKEN));
    options.insert("shortcuts", Value::from(shortcut_list(specs)));
    let results = request(conn, &handle, "CreateSession", &(options,)).await?;
    let session = results
        .get("session_handle")
        .ok_or_else(|| zbus::Error::Failure("CreateSession: no session_handle".to_string()))?;
    // Spec: a string; some portals send an object path.
    let path = match &**session {
        Value::Str(path) => path.to_string(),
        Value::ObjectPath(path) => path.to_string(),
        _ => {
            return Err(zbus::Error::Failure(
                "CreateSession: bad session_handle".to_string(),
            ))
        }
    };
    Ok(OwnedObjectPath::try_from(path)?)
}

/// Bind `specs` in `session` (the desktop may ask the user first).
///
/// # Errors
///
/// A D-Bus failure, or a portal response other than success.
pub(crate) async fn bind_shortcuts(
    conn: &zbus::Connection,
    session: &OwnedObjectPath,
    specs: &[Spec<'_>],
) -> zbus::Result<()> {
    let handle = token("req");
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", Value::from(handle.clone()));
    let body = (session.as_ref(), shortcut_list(specs), "", options);
    request(conn, &handle, "BindShortcuts", &body).await?;
    Ok(())
}

/// Close `session`; errors are ignored (the session may be gone).
pub(crate) async fn close_session(conn: &zbus::Connection, session: &OwnedObjectPath) {
    if let Ok(proxy) = zbus::Proxy::new(
        conn,
        DESKTOP,
        session.as_ref(),
        "org.freedesktop.portal.Session",
    )
    .await
    {
        let _: zbus::Result<()> = proxy.call("Close", &()).await;
    }
}

/// Why a held session may be gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lost {
    /// The portal closed this session.
    Session(OwnedObjectPath),
    /// The portal service restarted or went away: every session is gone.
    Portal,
}

impl Lost {
    /// Whether this ends `session`.
    pub(crate) fn ends(&self, session: Option<&OwnedObjectPath>) -> bool {
        match self {
            Self::Portal => true,
            Self::Session(path) => session == Some(path),
        }
    }
}

/// Session `Closed` signals and portal restarts, as one stream.
///
/// # Errors
///
/// A D-Bus failure while subscribing.
pub(crate) async fn lost_events(
    conn: &zbus::Connection,
) -> zbus::Result<std::pin::Pin<Box<dyn futures_util::Stream<Item = Lost> + Send>>> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.portal.Session")?
        .member("Closed")?
        .build();
    let closed = zbus::MessageStream::for_match_rule(rule, conn, None)
        .await?
        .filter_map(|message| async move {
            let message = message.ok()?;
            let path = message.header().path()?.to_owned();
            Some(Lost::Session(OwnedObjectPath::from(path)))
        });
    let owners = zbus::fdo::DBusProxy::new(conn)
        .await?
        .receive_name_owner_changed_with_args(&[(0, DESKTOP)])
        .await?
        // Only a departing owner ends our sessions; a portal starting up
        // (often started by our own call) does not.
        .filter_map(|change| async move {
            let args = change.args().ok()?;
            args.old_owner().as_ref().is_some().then_some(Lost::Portal)
        });
    Ok(Box::pin(futures_util::stream::select(closed, owners)))
}

/// One `Activated` or `Deactivated` signal.
#[derive(Debug)]
pub(crate) struct ShortcutSignal {
    /// `Deactivated` (the key was released) rather than `Activated`.
    pub released: bool,
    /// The session the shortcut belongs to.
    pub session: OwnedObjectPath,
    /// The shortcut id we bound.
    pub id: String,
    /// Signal options (`activation_token`, ...).
    pub options: HashMap<String, OwnedValue>,
}

/// Shortcut signals from whichever process owns the portal now. Matched by
/// interface only, not by sender: a subscription bound to the portal's
/// unique name would go deaf when the portal restarts.
///
/// # Errors
///
/// A D-Bus failure while subscribing.
pub(crate) async fn shortcut_signals(
    conn: &zbus::Connection,
) -> zbus::Result<std::pin::Pin<Box<dyn futures_util::Stream<Item = ShortcutSignal> + Send>>> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(INTERFACE)?
        .build();
    let stream = zbus::MessageStream::for_match_rule(rule, conn, None)
        .await?
        .filter_map(|message| async move {
            let message = message.ok()?;
            let released = match message.header().member()?.as_str() {
                "Activated" => false,
                "Deactivated" => true,
                _ => return None,
            };
            let (session, id, _timestamp, options): (
                OwnedObjectPath,
                String,
                u64,
                HashMap<String, OwnedValue>,
            ) = message.body().deserialize().ok()?;
            Some(ShortcutSignal {
                released,
                session,
                id,
                options,
            })
        });
    Ok(Box::pin(stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_list_carries_description_and_trigger() {
        let list = shortcut_list(&[("ctrl-alt-h", "Clipmonger popup", "CTRL+ALT+h")]);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0, "ctrl-alt-h");
        assert_eq!(list[0].1["description"], Value::from("Clipmonger popup"));
        assert_eq!(list[0].1["preferred_trigger"], Value::from("CTRL+ALT+h"));
    }

    #[test]
    fn only_our_session_or_a_portal_restart_ends_the_binding() {
        let ours =
            OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/session/1_2/clipmonger")
                .unwrap();
        let other =
            OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/session/1_2/other").unwrap();
        assert!(Lost::Session(ours.clone()).ends(Some(&ours)));
        assert!(!Lost::Session(other).ends(Some(&ours)));
        assert!(Lost::Portal.ends(Some(&ours)));
        assert!(Lost::Portal.ends(None));
    }

    #[test]
    fn tokens_are_unique_and_portal_safe() {
        let a = token("req");
        let b = token("req");
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    }
}
