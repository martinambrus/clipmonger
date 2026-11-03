#![cfg(unix)]
use clipmonger_core::Store;
use clipmonger_daemon::DaemonState;
use clipmonger_ipc::framing;
use clipmonger_ipc::proto::{Envelope, FrameKind, IPC_VERSION};
use clipmonger_ipc::server::{Handler, ServeOptions, Server};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

fn request(id: &str, method: &str, params: Value) -> Envelope {
    Envelope {
        v: IPC_VERSION.into(),
        id: id.into(),
        kind: FrameKind::Req,
        method: Some(method.into()),
        params: Some(params),
        result: None,
        error: None,
        seq: None,
    }
}
fn exchange(
    w: &mut UnixStream,
    r: &mut BufReader<UnixStream>,
    id: &str,
    method: &str,
    params: Value,
) -> Envelope {
    w.write_all(&framing::encode_line(&request(id, method, params)).unwrap())
        .unwrap();
    let mut line = String::new();
    assert!(r.read_line(&mut line).unwrap() > 0);
    assert!(line.len() < 256 * 1024);
    let response = framing::decode_line(line.trim_end().as_bytes()).unwrap();
    assert_eq!(response.id, id);
    response
}
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)] // Exercises the complete socket and cancellation sequence.
async fn compact_owned_socket_progress_cancel_and_legacy() {
    let dir = std::env::temp_dir().join(format!("compact-ipc-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&dir).unwrap();
    let store = Store::open(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("clipstore.sqlite3")).unwrap();
    db.execute_batch(
        "CREATE INDEX idx_clips_compact_history ON clips(removed,last_used_ms DESC,id DESC); CREATE INDEX IF NOT EXISTS idx_compact_payload_clip ON clip_payloads(clip_id)",
    )
    .unwrap();
    let id = uuid::Uuid::now_v7().to_string();
    db.execute("INSERT INTO clips(id,created_ms,source_os,preview,search_text,content_hash,rev_ms,rev_dev,last_used_ms) VALUES(?1,1,'test','hit','hit','x',1,'test',1)", [&id]).unwrap();
    let state = Arc::new(DaemonState::new(store));
    let handler: Handler = Arc::new(move |method, params| state.handle(&method, &params));
    let path = dir.join("sock");
    let server = Server::bind_with(&path, "token".into(), handler, ServeOptions::default())
        .await
        .unwrap();
    let mut w = UnixStream::connect(&path).unwrap();
    w.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut r = BufReader::new(w.try_clone().unwrap());
    assert!(exchange(
        &mut w,
        &mut r,
        "h",
        "hello",
        json!({"token":"token","client":"test"})
    )
    .error
    .is_none());
    let params = json!({"owner":7,"generation":9,"query":"hit","byte_budget":1,"candidate_budget":1,"row_budget":1});
    let first = exchange(&mut w, &mut r, "a", "history.compact", params.clone())
        .result
        .unwrap();
    assert_eq!(first["rows"], json!([]));
    assert_eq!(first["done"], false);
    assert_eq!(first["generation"], 9);
    let cursor = first["cursor"].as_str().unwrap().to_owned();
    let mut next = params.clone();
    next["cursor"] = json!(cursor);
    next["byte_budget"] = json!(65536);
    let wrong = {
        let mut p = next.clone();
        p["query"] = json!("other");
        p
    };
    assert_eq!(
        exchange(&mut w, &mut r, "w", "history.compact", wrong)
            .error
            .unwrap()
            .code,
        "bad_params"
    );
    let mut wrong_owner = next.clone();
    wrong_owner["owner"] = json!(8);
    assert_eq!(
        exchange(&mut w, &mut r, "o", "history.compact", wrong_owner)
            .error
            .unwrap()
            .code,
        "bad_params"
    );
    let result = exchange(&mut w, &mut r, "b", "history.compact", next)
        .result
        .unwrap();
    assert_eq!(result["rows"][0]["id"], id);
    let cancel = json!({"owner":7,"generation":10,"query":"hit","cursor":cursor});
    assert_eq!(
        exchange(
            &mut w,
            &mut r,
            "c",
            "history.compact.cancel",
            cancel.clone()
        )
        .result
        .unwrap()["cancelled"],
        true
    );
    assert_eq!(
        exchange(&mut w, &mut r, "d", "history.compact.cancel", cancel)
            .error
            .unwrap()
            .code,
        "stale_cursor"
    );
    let mut complete = params.clone();
    complete["byte_budget"] = json!(65536);
    complete["row_budget"] = json!(50);
    complete["candidate_budget"] = json!(32);
    let ended = exchange(&mut w, &mut r, "f", "history.compact", complete)
        .result
        .unwrap();
    assert_eq!(ended["done"], true);
    assert_eq!(ended["cursor"], Value::Null);
    assert_eq!(ended["rows"][0]["id"], id);
    assert!(exchange(&mut w, &mut r, "e", "history.compact", json!({}))
        .error
        .is_some());
    assert_eq!(
        exchange(&mut w, &mut r, "l", "history.list", json!({}))
            .result
            .unwrap()["clips"][0]["id"],
        id
    );
    let oversized_id = uuid::Uuid::now_v7().to_string();
    db.execute("INSERT INTO clips(id,created_ms,source_os,preview,search_text,content_hash,rev_ms,rev_dev,last_used_ms) VALUES(?1,1,'test','oversized','oversized','x',1,'test',2)", [&oversized_id]).unwrap();
    db.execute(
        "INSERT INTO clip_payloads(clip_id,mime) VALUES(?1,?2)",
        rusqlite::params![oversized_id, "x".repeat(257)],
    )
    .unwrap();
    let mut bad = params.clone();
    bad["query"] = json!("oversized");
    bad["byte_budget"] = json!(65536);
    bad["candidate_budget"] = json!(32);
    assert_eq!(
        exchange(&mut w, &mut r, "m", "history.compact", bad)
            .error
            .unwrap()
            .code,
        "metadata_too_large"
    );
    drop(server);
    drop(db);
    drop(r);
    drop(w);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn direct_uuid_lookup_reaches_rows_beyond_legacy_limit() {
    let dir = std::env::temp_dir().join(format!("compact-lookup-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&dir).unwrap();
    let store = Store::open(&dir).unwrap();
    let mut db = rusqlite::Connection::open(dir.join("clipstore.sqlite3")).unwrap();
    let tx = db.transaction().unwrap();
    let first = "00000000-0000-0000-0000-000000000001";
    {
        let mut stmt = tx.prepare("INSERT INTO clips(id,created_ms,source_os,preview,search_text,content_hash,rev_ms,rev_dev,last_used_ms) VALUES(?1,1,'test','','','x',1,'test',?2)").unwrap();
        for n in 1..=10001 {
            stmt.execute(rusqlite::params![
                format!("00000000-0000-0000-0000-{n:012x}"),
                n
            ])
            .unwrap();
        }
    }
    tx.commit().unwrap();
    let state = DaemonState::new(store);
    assert_eq!(
        state
            .handle("history.delete", &json!({"clip":first}))
            .unwrap()["deleted"],
        true
    );
    assert_eq!(
        state
            .handle("history.delete", &json!({"clip":first}))
            .unwrap()["deleted"],
        true
    );
    assert!(state
        .handle("history.delete", &json!({"clip":"not-a-uuid"}))
        .is_err());
    drop(state);
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}
