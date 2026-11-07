//! Local protocol regressions, not evidence of live-provider atomicity.
use super::*;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

#[derive(Debug)]
struct Request {
    url: reqwest::Url,
    body: Vec<u8>,
}

struct Model {
    destination: Option<Value>,
    requests: Vec<Request>,
}

struct FakeApi {
    adapter: PCloudAdapter,
    model: Arc<Mutex<Model>>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for FakeApi {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl FakeApi {
    async fn new(destination: Value) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let model = Arc::new(Mutex::new(Model {
            destination: Some(destination),
            requests: Vec::new(),
        }));
        let state = model.clone();
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                let url = reqwest::Url::parse(&format!(
                    "http://localhost{}",
                    line.split_whitespace().nth(1).unwrap()
                ))
                .unwrap();
                let mut length = 0;
                let mut chunked = false;
                loop {
                    line.clear();
                    stream.read_line(&mut line).await.unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    let (name, value) = line.split_once(':').unwrap();
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                    if name.eq_ignore_ascii_case("transfer-encoding") {
                        chunked = value.trim() == "chunked";
                    }
                }
                let mut body = Vec::new();
                if chunked {
                    loop {
                        line.clear();
                        stream.read_line(&mut line).await.unwrap();
                        let size = usize::from_str_radix(line.trim(), 16).unwrap();
                        if size == 0 {
                            line.clear();
                            stream.read_line(&mut line).await.unwrap();
                            assert_eq!(line, "\r\n");
                            break;
                        }
                        assert!(body.len() + size < 65536, "bounded test payload");
                        let offset = body.len();
                        body.resize(offset + size, 0);
                        stream.read_exact(&mut body[offset..]).await.unwrap();
                        let mut crlf = [0; 2];
                        stream.read_exact(&mut crlf).await.unwrap();
                        assert_eq!(&crlf, b"\r\n");
                    }
                } else {
                    assert!(length < 65536, "bounded test payload");
                    body.resize(length, 0);
                    stream.read_exact(&mut body).await.unwrap();
                }
                let response = {
                    let mut model = state.lock().unwrap();
                    let response = match url.path() {
                        "/listfolder" => json!({"result": 0, "metadata": {
                            "name": "root", "folderid": 0, "isfolder": true,
                            "contents": model.destination.iter().collect::<Vec<_>>()
                        }}),
                        // Model deletions faithfully: the old adapter would lose the
                        // destination here, before the subsequent injected failure.
                        "/deletefile" | "/deletefolderrecursive" => {
                            model.destination = None;
                            json!({"result": 0})
                        }
                        "/uploadfile" => {
                            if !url.query_pairs().any(|(k, v)| k == "nopartial" && v == "1") {
                                model.destination = None; // incomplete upload installed
                            }
                            json!({"result": 2000, "error": "injected upload failure"})
                        }
                        "/copyfile" | "/renamefile" | "/copyfolder" | "/renamefolder" => {
                            json!({"result": 2000, "error": "injected operation failure"})
                        }
                        path => panic!("unexpected pCloud request: {path}"),
                    };
                    model.requests.push(Request { url, body });
                    response.to_string()
                };
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            response.len(), response
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        });
        Self {
            adapter: PCloudAdapter::new_with_test_base(
                "synthetic-token".into(),
                BackendId("pcloud:test".into()),
                format!("http://{address}"),
            ),
            model,
            server,
        }
    }

    fn assert_preserved(&self, destination: &Value, paths: &[&str]) {
        let model = self.model.lock().unwrap();
        assert_eq!(model.destination.as_ref(), Some(destination));
        assert!(model.requests.iter().all(|request| !matches!(
            request.url.path(),
            "/deletefile" | "/deletefolderrecursive"
        )));
        assert_eq!(
            model
                .requests
                .iter()
                .map(|r| r.url.path())
                .collect::<Vec<_>>(),
            paths
        );
    }
}

fn object(stable_id: &str, name: &str) -> ObjectRef {
    ObjectRef {
        key: ObjectKey {
            backend_id: BackendId("pcloud:test".into()),
            stable_id: StableId(stable_id.into()),
        },
        parent_stable_id: Some(StableId("folder:0".into())),
        display_path: name.into(),
    }
}

#[derive(Clone, Copy, Debug)]
enum Action {
    Buffered,
    Streamed(Option<u64>),
    Copy,
    Move,
}

const ACTIONS: [Action; 5] = [
    Action::Buffered,
    Action::Streamed(Some(3)),
    Action::Streamed(None),
    Action::Copy,
    Action::Move,
];

impl Action {
    async fn run(
        self,
        api: &FakeApi,
        src: &ObjectRef,
        dst: &ObjectRef,
        on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError> {
        match self {
            Self::Buffered => {
                api.adapter
                    .write_object(dst, b"new".to_vec(), on_conflict)
                    .await
            }
            Self::Streamed(length) => {
                api.adapter
                    .write_from_reader(
                        dst,
                        Box::pin(std::io::Cursor::new(b"new")),
                        length,
                        on_conflict,
                    )
                    .await
            }
            Self::Copy => {
                api.adapter
                    .copy(src, dst, CopyOptions { on_conflict })
                    .await
            }
            Self::Move => {
                api.adapter
                    .move_object(src, dst, MoveOptions { on_conflict })
                    .await
            }
        }
    }

    fn endpoint(self) -> &'static str {
        match self {
            Self::Buffered | Self::Streamed(_) => "/uploadfile",
            Self::Copy => "/copyfile",
            Self::Move => "/renamefile",
        }
    }
}

fn old_file() -> Value {
    json!({"name": "target.txt", "fileid": 2, "isfolder": false, "size": 3,
        "contents_for_test": "old"})
}

#[tokio::test]
async fn overwrite_failures_preserve_existing_files_without_deletes() {
    for action in ACTIONS {
        let old = old_file();
        let api = FakeApi::new(old.clone()).await;
        let result = action
            .run(
                &api,
                &object("file:1", "source.txt"),
                &object("file:2", "target.txt"),
                ConflictResolution::Overwrite,
            )
            .await;
        assert!(
            result.is_err(),
            "{action:?} must propagate the injected failure"
        );
        api.assert_preserved(&old, &["/listfolder", action.endpoint()]);
        let model = api.model.lock().unwrap();
        let request = &model.requests[1];
        let params = request
            .url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        if action.endpoint() == "/uploadfile" {
            assert_eq!(params.get("nopartial").map(|v| v.as_ref()), Some("1"));
            assert!(String::from_utf8_lossy(&request.body).contains("filename=\"target.txt\""));
        } else {
            assert_eq!(params.get("toname").map(|v| v.as_ref()), Some("target.txt"));
        }
    }
}

#[tokio::test]
async fn overwrite_rejects_existing_empty_and_nonempty_folders_without_mutation() {
    for contents in [
        json!([]),
        json!([{"name": "child", "fileid": 3, "isfolder": false}]),
    ] {
        let old = json!({"name": "target.txt", "folderid": 2, "isfolder": true,
            "contents": contents});
        for action in ACTIONS {
            for source_id in ["file:1", "folder:1"] {
                let api = FakeApi::new(old.clone()).await;
                let result = action
                    .run(
                        &api,
                        &object(source_id, "source.txt"),
                        &object("folder:2", "target.txt"),
                        ConflictResolution::Overwrite,
                    )
                    .await;
                assert!(
                    matches!(result, Err(BackendError::Conflict { .. })),
                    "{action:?}: {result:?}"
                );
                api.assert_preserved(&old, &["/listfolder"]);
            }
        }
    }
}

#[tokio::test]
async fn same_object_copy_and_move_are_noops_before_any_request() {
    let api = FakeApi::new(old_file()).await;
    for source_id in ["file:1", "folder:1"] {
        let src = object(source_id, "source");
        // Identity is the key, not the display path.
        let dst = object(source_id, "other-display-path");
        for policy in [
            ConflictResolution::Overwrite,
            ConflictResolution::Fail,
            ConflictResolution::Skip,
            ConflictResolution::Rename,
        ] {
            let copy = Action::Copy
                .run(&api, &src, &dst, policy.clone())
                .await
                .unwrap();
            let moved = Action::Move.run(&api, &src, &dst, policy).await.unwrap();
            assert!(matches!(copy.kind, OperationKind::Copy));
            assert!(matches!(moved.kind, OperationKind::Move));
            assert_ne!(copy.id, moved.id);
        }
    }
    api.assert_preserved(&old_file(), &[]);
}

#[tokio::test]
async fn other_conflict_policies_keep_the_destination_and_resolve_names() {
    for action in ACTIONS {
        for policy in [
            ConflictResolution::Fail,
            ConflictResolution::Skip,
            ConflictResolution::Rename,
        ] {
            let old = old_file();
            let api = FakeApi::new(old.clone()).await;
            let result = action
                .run(
                    &api,
                    &object("file:1", "source.txt"),
                    &object("file:2", "target.txt"),
                    policy.clone(),
                )
                .await;
            if matches!(policy, ConflictResolution::Rename) {
                assert!(result.is_err()); // injected operation failure after choosing a free name
                api.assert_preserved(&old, &["/listfolder", action.endpoint()]);
                let model = api.model.lock().unwrap();
                let request = &model.requests[1];
                if action.endpoint() == "/uploadfile" {
                    assert!(String::from_utf8_lossy(&request.body)
                        .contains("filename=\"target (1).txt\""));
                } else {
                    assert!(request
                        .url
                        .query_pairs()
                        .any(|(k, v)| k == "toname" && v == "target (1).txt"));
                }
            } else {
                assert!(matches!(result, Err(BackendError::Conflict { .. })));
                api.assert_preserved(&old, &["/listfolder"]);
            }
        }
    }
}
