use super::*;
use aws_sdk_s3::config::http::{HttpRequest, HttpResponse};
use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_s3::primitives::SdkBody;
use aws_smithy_runtime_api::client::http::{
    HttpClient, HttpConnector, HttpConnectorFuture, HttpConnectorSettings, SharedHttpConnector,
};
use aws_smithy_runtime_api::client::result::ConnectorError;
use aws_smithy_runtime_api::client::runtime_components::RuntimeComponents;
use std::collections::VecDeque;
use std::sync::Mutex;

#[derive(Clone, Debug)]
struct MockHttp {
    responses: Arc<Mutex<VecDeque<Result<HttpResponse, ConnectorError>>>>,
    requests: Arc<Mutex<Vec<(String, String)>>>,
    conditions: Arc<Mutex<Vec<Option<String>>>>,
}

impl HttpClient for MockHttp {
    fn http_connector(
        &self,
        _: &HttpConnectorSettings,
        _: &RuntimeComponents,
    ) -> SharedHttpConnector {
        SharedHttpConnector::new(self.clone())
    }
}

impl HttpConnector for MockHttp {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        self.conditions
            .lock()
            .unwrap()
            .push(request.headers().get("if-none-match").map(str::to_owned));
        self.requests
            .lock()
            .unwrap()
            .push((request.method().to_string(), request.uri().to_string()));
        HttpConnectorFuture::ready(
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected S3 request (possibly a write after a failed HEAD)"),
        )
    }
}

fn response(status: u16, body: &'static str) -> Result<HttpResponse, ConnectorError> {
    Ok(HttpResponse::new(
        status.try_into().unwrap(),
        SdkBody::from(body),
    ))
}

fn adapter(responses: Vec<Result<HttpResponse, ConnectorError>>) -> (S3Adapter, MockHttp) {
    let http = MockHttp {
        responses: Arc::new(Mutex::new(responses.into())),
        requests: Default::default(),
        conditions: Default::default(),
    };
    let config = aws_sdk_s3::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new("us-east-1"))
        .credentials_provider(Credentials::new(
            "synthetic-key",
            "synthetic-secret",
            None,
            None,
            "test",
        ))
        .endpoint_url("https://s3.test")
        .force_path_style(true)
        .retry_config(aws_sdk_s3::config::retry::RetryConfig::disabled())
        .http_client(http.clone())
        .build();
    let adapter = S3Adapter::new(
        S3Config {
            bucket: "archive".into(),
            prefix: None,
            endpoint_url: Some("https://s3.test".into()),
            custom_endpoint_conditional_writes_confirmed: true,
            region: "us-east-1".into(),
        },
        BackendId("s3:test".into()),
    );
    adapter.client.set(S3Client::from_conf(config)).unwrap();
    (adapter, http)
}

fn object(path: &str) -> ObjectRef {
    ObjectRef {
        key: ObjectKey {
            backend_id: BackendId("s3:test".into()),
            stable_id: StableId(path.into()),
        },
        parent_stable_id: None,
        display_path: path.into(),
    }
}

#[derive(Clone, Copy, Debug)]
enum Write {
    Bytes,
    Reader,
    Copy,
    Multipart,
}

impl Write {
    async fn run(
        self,
        adapter: &S3Adapter,
        policy: ConflictResolution,
    ) -> Result<(), BackendError> {
        let dst = object("file.txt");
        match self {
            Self::Bytes => adapter.write_object(&dst, vec![], policy).await.map(|_| ()),
            Self::Reader => adapter
                .write_from_reader(&dst, Box::pin(tokio::io::empty()), Some(0), policy)
                .await
                .map(|_| ()),
            Self::Copy => adapter
                .copy(
                    &object("source.txt"),
                    &dst,
                    CopyOptions {
                        on_conflict: policy,
                    },
                )
                .await
                .map(|_| ()),
            Self::Multipart => adapter
                .create_multipart_upload(&dst, policy)
                .await
                .map(|_| ()),
        }
    }

    fn success(self) -> Result<HttpResponse, ConnectorError> {
        response(
            200,
            match self {
                Self::Bytes | Self::Reader => "",
                Self::Copy => "<CopyObjectResult><ETag>test</ETag></CopyObjectResult>",
                Self::Multipart => "<InitiateMultipartUploadResult><UploadId>test</UploadId></InitiateMultipartUploadResult>",
            },
        )
    }
}

const WRITES: [Write; 4] = [Write::Bytes, Write::Reader, Write::Copy, Write::Multipart];
const POLICIES: [ConflictResolution; 3] = [
    ConflictResolution::Fail,
    ConflictResolution::Skip,
    ConflictResolution::Rename,
];

fn assert_heads_only(http: &MockHttp, count: usize) {
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), count);
    assert!(requests.iter().all(|(method, _)| method == "HEAD"));
    assert!(http.responses.lock().unwrap().is_empty());
}

#[tokio::test]
async fn uncertain_head_stops_every_non_overwrite_write() {
    for policy in POLICIES {
        for write in WRITES {
            // Also check failures while searching for a renamed destination.
            for existing in [false, true] {
                if existing && !matches!(policy, ConflictResolution::Rename) {
                    continue;
                }
                for failure in [403, 500, 0] {
                    let mut responses = Vec::new();
                    if existing {
                        responses.push(response(200, ""));
                    }
                    responses.push(match failure {
                        0 => Err(ConnectorError::io(
                            std::io::Error::new(std::io::ErrorKind::ConnectionReset, "test reset")
                                .into(),
                        )),
                        status => response(status, ""),
                    });
                    let (adapter, http) = adapter(responses);
                    let result = write.run(&adapter, policy.clone()).await;
                    match result {
                        Err(BackendError::Network(detail)) => {
                            assert!(detail.message.starts_with("S3 head_object failed:"));
                        }
                        result => panic!("{policy:?} {write:?} {failure}: {result:?}"),
                    }
                    assert_heads_only(&http, if existing { 2 } else { 1 });
                }
            }
        }
    }
}

#[tokio::test]
async fn definitive_404_allows_non_overwrite_writes() {
    for policy in POLICIES {
        for write in WRITES {
            let (adapter, http) = adapter(vec![response(404, ""), write.success()]);
            write.run(&adapter, policy.clone()).await.unwrap();
            let requests = http.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0].0, "HEAD");
            assert_ne!(requests[1].0, "HEAD");
            assert!(requests[1].1.contains("/archive/file.txt"));
            assert!(http.responses.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn existing_objects_keep_conflict_and_rename_behavior() {
    for write in WRITES {
        for policy in [ConflictResolution::Fail, ConflictResolution::Skip] {
            let (adapter, http) = adapter(vec![response(200, "")]);
            assert!(matches!(
                write.run(&adapter, policy).await,
                Err(BackendError::Conflict { object }) if object.display_path == "file.txt"
            ));
            assert_heads_only(&http, 1);
        }
        let (adapter, http) = adapter(vec![
            response(200, ""),
            response(200, ""),
            response(404, ""),
            write.success(),
        ]);
        write
            .run(&adapter, ConflictResolution::Rename)
            .await
            .unwrap();
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert!(requests[..3].iter().all(|(method, _)| method == "HEAD"));
        assert!(requests[1].1.ends_with("/file%20%281%29.txt"));
        assert!(requests[2].1.ends_with("/file%20%282%29.txt"));
        assert!(requests[3].1.contains("/file%20%282%29.txt"));
        assert!(http.responses.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn overwrite_does_not_depend_on_head() {
    for write in WRITES {
        let (mut adapter, http) = adapter(vec![write.success()]);
        adapter.config.custom_endpoint_conditional_writes_confirmed = false;
        write
            .run(&adapter, ConflictResolution::Overwrite)
            .await
            .unwrap();
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(http.conditions.lock().unwrap().iter().all(Option::is_none));
        assert_ne!(requests[0].0, "HEAD");
        assert!(requests[0].1.contains("/archive/file.txt"));
        assert!(http.responses.lock().unwrap().is_empty());
    }
}

// Simulate a creator winning after HEAD: final conditional writes must reject
// the race, never fall back to an unconditional write (including Rename).
#[tokio::test]
async fn final_writes_are_conditional_except_overwrite() {
    for default_aws in [false, true] {
        for policy in POLICIES.into_iter().chain([ConflictResolution::Overwrite]) {
            for write in WRITES {
                for status in [200, 409, 412, 501] {
                    let overwrite = matches!(policy, ConflictResolution::Overwrite);
                    let mut responses = Vec::new();
                    if !overwrite {
                        responses.push(response(404, ""));
                    }
                    if matches!(write, Write::Multipart) {
                        responses.push(write.success());
                    }
                    responses.push(if status == 200 {
                    if matches!(write, Write::Multipart) {
                        response(200, "<CompleteMultipartUploadResult><ETag>test</ETag></CompleteMultipartUploadResult>")
                    } else {
                        write.success()
                    }
                } else {
                    response(status, "<Error><Code>ConditionalRequestConflict</Code></Error>")
                });
                    let expected_requests = responses.len();
                    let (mut adapter, http) = adapter(responses);
                    if default_aws {
                        adapter.config.endpoint_url = None;
                        adapter.config.custom_endpoint_conditional_writes_confirmed = false;
                    } else if overwrite {
                        adapter.config.custom_endpoint_conditional_writes_confirmed = false;
                    }
                    let result = if matches!(write, Write::Multipart) {
                        let session = adapter
                            .create_multipart_upload(&object("file.txt"), policy.clone())
                            .await
                            .unwrap();
                        adapter
                            .complete_multipart_upload(&session, vec![], policy.clone())
                            .await
                            .map(|_| ())
                    } else {
                        write.run(&adapter, policy.clone()).await
                    };
                    match (status, overwrite) {
                        (200, _) => result.unwrap(),
                        (409 | 412, false) => assert!(
                            matches!(&result, Err(BackendError::Conflict { object }) if object.display_path == "file.txt"),
                            "{policy:?} {write:?} {status}: {result:?}"
                        ),
                        (409 | 412, true) => assert!(
                            matches!(&result, Err(BackendError::Network(_))),
                            "{policy:?} {write:?} {status}: {result:?}"
                        ),
                        _ => assert!(matches!(result, Err(BackendError::Network(_)))),
                    }
                    let conditions = http.conditions.lock().unwrap();
                    assert_eq!(
                        conditions.len(),
                        expected_requests,
                        "{policy:?} {write:?} {status}"
                    );
                    assert_eq!(
                        conditions.last().unwrap().as_deref(),
                        if overwrite { None } else { Some("*") }
                    );
                    assert!(conditions[..conditions.len() - 1]
                        .iter()
                        .all(Option::is_none));
                    assert!(http.responses.lock().unwrap().is_empty());
                }
            }
        }
    }
}

#[tokio::test]
async fn streamed_multipart_keeps_policy_and_aborts_after_conflict() {
    for policy in [ConflictResolution::Fail, ConflictResolution::Overwrite] {
        for status in [200, 412] {
            let mut responses = vec![
                Write::Multipart.success(),
                response(200, ""), // UploadPart
                response(
                    status,
                    if status == 200 {
                        "<CompleteMultipartUploadResult><ETag>test</ETag></CompleteMultipartUploadResult>"
                    } else {
                        "<Error><Code>PreconditionFailed</Code></Error>"
                    },
                ),
            ];
            if status == 412 {
                responses.push(response(204, "")); // AbortMultipartUpload
            }
            let (adapter, http) = adapter(responses);
            let result = adapter
                .upload_multipart_chunks(
                    "file.txt",
                    vec![b"abc".to_vec()],
                    Box::pin(tokio::io::empty()),
                    &policy,
                )
                .await;
            let overwrite = matches!(policy, ConflictResolution::Overwrite);
            if status == 412 && !overwrite {
                assert!(
                    matches!(&result, Err(BackendError::Conflict { .. })),
                    "{policy:?} {status}: {result:?}"
                );
            } else if status == 412 {
                assert!(
                    matches!(&result, Err(BackendError::Network(_))),
                    "{policy:?} {status}: {result:?}"
                );
            } else {
                result.unwrap();
            }
            let conditions = http.conditions.lock().unwrap();
            assert_eq!(
                conditions[2].as_deref(),
                if matches!(policy, ConflictResolution::Overwrite) {
                    None
                } else {
                    Some("*")
                }
            );
            assert_eq!(conditions.len(), if status == 412 { 4 } else { 3 });
            assert!(http.responses.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn list_parts_maps_only_no_such_upload_to_not_found() {
    use iris_core_domain::types::multipart::MultipartUploadSession;

    let session = MultipartUploadSession {
        destination: object("file.txt"),
        upload_id: "missing-upload".into(),
    };
    let (s3, http) = adapter(vec![response(
        404,
        "<Error><Code>NoSuchUpload</Code><Message>The specified multipart upload does not exist.</Message></Error>",
    )]);
    assert!(matches!(
        s3.list_multipart_parts(&session).await,
        Err(BackendError::NotFound { object }) if object.display_path == "file.txt"
    ));
    assert!(http.responses.lock().unwrap().is_empty());

    for failure in [
        response(403, "<Error><Code>AccessDenied</Code></Error>"),
        response(500, "<Error><Code>InternalError</Code></Error>"),
    ] {
        let (s3, http) = adapter(vec![failure]);
        assert!(matches!(
            s3.list_multipart_parts(&session).await,
            Err(BackendError::Network(_))
        ));
        assert!(http.responses.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn abort_is_idempotent_only_for_no_such_upload() {
    use iris_core_domain::types::multipart::MultipartUploadSession;
    let session = MultipartUploadSession {
        destination: object("file.txt"),
        upload_id: "missing-upload".into(),
    };
    // The definitive already-absent upload error must clean up without
    // retaining retry state.
    {
        let (s3, http) = adapter(vec![response(
            404,
            "<Error><Code>NoSuchUpload</Code><Message>The specified multipart upload does not exist.</Message></Error>",
        )]);
        s3.abort_multipart_upload(&session)
            .await
            .expect("NoSuchUpload retries must succeed");
        assert!(http.responses.lock().unwrap().is_empty());
    }

    // Permission, service, and transport failures must stay retryable.
    for failure in [
        response(403, "<Error><Code>AccessDenied</Code></Error>"),
        response(500, "<Error><Code>InternalError</Code></Error>"),
    ] {
        let (s3, http) = adapter(vec![failure]);
        let result = s3.abort_multipart_upload(&session).await;
        assert!(
            matches!(result, Err(BackendError::Network(_))),
            "unexpected abort result: {result:?}"
        );
        assert!(http.responses.lock().unwrap().is_empty());
    }
    let io_error: Result<HttpResponse, _> =
        Err(aws_smithy_runtime_api::client::result::ConnectorError::io(
            std::io::Error::new(std::io::ErrorKind::ConnectionReset, "test reset").into(),
        ));
    {
        let (s3, _) = adapter(vec![io_error]);
        assert!(matches!(
            s3.abort_multipart_upload(&session).await,
            Err(BackendError::Network(_))
        ));
    }
}

#[tokio::test]
async fn unconfirmed_custom_endpoint_stops_before_any_request() {
    for policy in POLICIES {
        let (mut adapter, http) = adapter(vec![]);
        adapter.config.custom_endpoint_conditional_writes_confirmed = false;
        for write in WRITES {
            assert!(matches!(
                write.run(&adapter, policy.clone()).await,
                Err(BackendError::Unsupported { .. })
            ));
        }
        // Resumed uploads must not bypass the guard at completion.
        let session = MultipartUploadSession {
            destination: object("file.txt"),
            upload_id: "existing-upload".into(),
        };
        assert!(matches!(
            adapter
                .complete_multipart_upload(&session, vec![], policy.clone())
                .await,
            Err(BackendError::Unsupported { .. })
        ));
        assert!(matches!(
            adapter
                .upload_multipart_chunks("file.txt", vec![], Box::pin(tokio::io::empty()), &policy)
                .await,
            Err(BackendError::Unsupported { .. })
        ));
        assert!(http.requests.lock().unwrap().is_empty());
    }
}
