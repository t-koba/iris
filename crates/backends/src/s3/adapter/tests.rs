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
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Mutex;

#[derive(Clone, Debug)]
struct MockHttp {
    responses: Arc<Mutex<VecDeque<Result<HttpResponse, ConnectorError>>>>,
    requests: Arc<Mutex<Vec<(String, String)>>>,
    conditions: Arc<Mutex<Vec<Option<String>>>>,
    bodies: Arc<Mutex<Vec<(usize, u64)>>>,
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
        let response = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected S3 request (possibly a write after a failed HEAD)");
        let bodies = self.bodies.clone();
        HttpConnectorFuture::new(async move {
            let body = aws_sdk_s3::primitives::ByteStream::new(request.into_body())
                .collect()
                .await
                .unwrap()
                .into_bytes();
            bodies.lock().unwrap().push(body_signature(&body));
            response
        })
    }
}

fn body_signature(body: &[u8]) -> (usize, u64) {
    let mut hash = DefaultHasher::new();
    body.hash(&mut hash);
    (body.len(), hash.finish())
}

fn response(status: u16, body: &'static str) -> Result<HttpResponse, ConnectorError> {
    Ok(HttpResponse::new(
        status.try_into().unwrap(),
        SdkBody::from(body),
    ))
}

fn adapter(responses: Vec<Result<HttpResponse, ConnectorError>>) -> (S3Adapter, MockHttp) {
    adapter_with_prefix(responses, None)
}

fn adapter_with_prefix(
    responses: Vec<Result<HttpResponse, ConnectorError>>,
    prefix: Option<&str>,
) -> (S3Adapter, MockHttp) {
    let http = MockHttp {
        responses: Arc::new(Mutex::new(responses.into())),
        requests: Default::default(),
        conditions: Default::default(),
        bodies: Default::default(),
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
            prefix: prefix.map(str::to_owned),
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
        assert!(requests[1].1.contains("/file%20%281%29.txt"));
        assert!(requests[2].1.contains("/file%20%282%29.txt"));
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

// Non-retryable outcomes must never fall back to an unconditional write.
// Retryable cases and their budgets are covered separately below.
#[tokio::test]
async fn final_writes_are_conditional_except_overwrite() {
    for default_aws in [false, true] {
        for policy in POLICIES.into_iter().chain([ConflictResolution::Overwrite]) {
            for write in WRITES {
                for status in [200, 409, 412, 501] {
                    let overwrite = matches!(policy, ConflictResolution::Overwrite);
                    if !overwrite
                        && ((status == 409 && matches!(write, Write::Bytes | Write::Reader))
                            || (status == 412
                                && matches!(policy, ConflictResolution::Rename)
                                && !matches!(write, Write::Multipart)))
                    {
                        continue;
                    }
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
                        (412, false) => assert!(
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
    for policy in POLICIES.into_iter().chain([ConflictResolution::Overwrite]) {
        for status in [200, 409, 412] {
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
            if status != 200 {
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
            } else if status != 200 {
                if status == 409 && !overwrite {
                    assert!(matches!(&result, Err(BackendError::Network(detail))
                        if detail.message.contains("new multipart upload") && detail.message.contains("409")));
                }
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
            assert_eq!(conditions.len(), if status != 200 { 4 } else { 3 });
            if status != 200 {
                assert_eq!(http.requests.lock().unwrap()[3].0, "DELETE");
            }
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

const PREFIX_LIST_RESPONSE: &str = r#"<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Name>archive</Name><Prefix>foo/</Prefix><KeyCount>2</KeyCount><MaxKeys>1000</MaxKeys>
  <Delimiter>/</Delimiter><IsTruncated>false</IsTruncated>
  <Contents><Key>foo/file.txt</Key><ETag>&quot;file-etag&quot;</ETag><Size>7</Size></Contents>
  <CommonPrefixes><Prefix>foo/team/</Prefix></CommonPrefixes>
</ListBucketResult>"#;

#[tokio::test]
async fn configured_prefix_is_a_slash_delimited_root_for_listing_and_keys() {
    for configured_prefix in ["foo", "foo/"] {
        let (adapter, http) = adapter_with_prefix(
            vec![response(200, PREFIX_LIST_RESPONSE)],
            Some(configured_prefix),
        );
        assert_eq!(adapter.config.prefix.as_deref(), Some("foo/"));
        let page = adapter.list_names(&object(""), None).await.unwrap();

        assert!(http.requests.lock().unwrap()[0].1.contains("prefix=foo%2F"));
        assert_eq!(page.entries.len(), 2);
        let paths: Vec<_> = page
            .entries
            .iter()
            .map(|entry| match entry {
                ListedEntry::WithMeta(entry) => (
                    entry.name.name.as_str(),
                    entry.name.object_ref.display_path.as_str(),
                ),
                ListedEntry::NameOnly(_) => panic!("S3 list entries include metadata"),
            })
            .collect();
        assert!(paths.contains(&("file.txt", "file.txt")));
        assert!(paths.contains(&("team", "team/")));
        assert_eq!(
            adapter.object_key(&object("team/file.txt")),
            "foo/team/file.txt"
        );
        assert_eq!(
            adapter.display_path_from_key("foo/team/file.txt").unwrap(),
            "team/file.txt"
        );
    }
}

#[tokio::test]
async fn configured_prefix_rejects_sibling_prefix_results() {
    let sibling_responses = [
        r#"<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
          <Name>archive</Name><Prefix>foo/</Prefix><KeyCount>1</KeyCount><MaxKeys>1000</MaxKeys>
          <Delimiter>/</Delimiter><IsTruncated>false</IsTruncated>
          <CommonPrefixes><Prefix>foobar/</Prefix></CommonPrefixes>
        </ListBucketResult>"#,
        r#"<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
          <Name>archive</Name><Prefix>foo/</Prefix><KeyCount>1</KeyCount><MaxKeys>1000</MaxKeys>
          <Delimiter>/</Delimiter><IsTruncated>false</IsTruncated>
          <Contents><Key>foobar/private.jpg</Key><Size>10</Size></Contents>
        </ListBucketResult>"#,
    ];

    for body in sibling_responses {
        let (adapter, http) = adapter_with_prefix(vec![response(200, body)], Some("foo"));
        assert!(matches!(
            adapter.list_names(&object(""), None).await,
            Err(BackendError::Internal(_))
        ));
        assert!(http.requests.lock().unwrap()[0].1.contains("prefix=foo%2F"));
    }
}

#[tokio::test]
async fn unprefixed_listing_and_keys_remain_unprefixed() {
    let body = r#"<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
      <Name>archive</Name><Prefix></Prefix><KeyCount>1</KeyCount><MaxKeys>1000</MaxKeys>
      <Delimiter>/</Delimiter><IsTruncated>false</IsTruncated>
      <Contents><Key>file.txt</Key><Size>7</Size></Contents>
    </ListBucketResult>"#;
    for prefix in [None, Some("")] {
        let (adapter, http) = adapter_with_prefix(vec![response(200, body)], prefix);
        let page = adapter.list_names(&object(""), None).await.unwrap();

        assert_eq!(adapter.object_key(&object("file.txt")), "file.txt");
        assert_eq!(page.entries.len(), 1);
        assert!(matches!(
            &page.entries[0],
            ListedEntry::WithMeta(entry) if entry.name.object_ref.display_path == "file.txt"
        ));
        assert!(!http.requests.lock().unwrap()[0].1.contains("prefix=foo"));
    }
}

#[tokio::test]
async fn buffered_put_retries_409_and_rename_412_with_same_body() {
    for status in [409, 412] {
        for source in 0..3 {
            // Vec, known-length reader, unknown-length small reader
            for policy in POLICIES {
                if status == 412 && !matches!(policy, ConflictResolution::Rename) {
                    continue; // Fail/Skip's definite conflict is covered by the status matrix.
                }
                let (s3, http) = adapter(vec![
                    response(404, ""),
                    response(
                        status,
                        "<Error><Code>ConditionalRequestConflict</Code></Error>",
                    ),
                    response(404, ""), // Re-resolve original (409) or next suffix (412).
                    response(200, ""),
                ]);
                let data = b"replay these bytes".to_vec();
                if source == 0 {
                    s3.write_object(&object("file.txt"), data.clone(), policy.clone())
                        .await
                        .unwrap();
                } else {
                    s3.write_from_reader(
                        &object("file.txt"),
                        Box::pin(std::io::Cursor::new(data.clone())),
                        (source == 1).then_some(data.len() as u64),
                        policy.clone(),
                    )
                    .await
                    .unwrap();
                }
                let requests = http.requests.lock().unwrap();
                assert_eq!(requests.len(), 4);
                assert_eq!(requests[2].0, "HEAD");
                assert!(requests[3].1.contains(if status == 412 {
                    "/file%20%281%29.txt"
                } else {
                    "/file.txt"
                }));
                assert_eq!(
                    *http.conditions.lock().unwrap(),
                    [None, Some("*".into()), None, Some("*".into())]
                );
                let bodies = http.bodies.lock().unwrap();
                assert_eq!(bodies[1], body_signature(&data));
                assert_eq!(bodies[3], bodies[1]);
                assert!(http.responses.lock().unwrap().is_empty());
            }
        }
    }
}

#[tokio::test]
async fn put_409_reresolves_under_the_same_policy_and_has_a_retry_budget() {
    for policy in POLICIES {
        let mut responses = vec![response(404, ""), response(409, ""), response(200, "")];
        let rename = matches!(policy, ConflictResolution::Rename);
        if rename {
            responses.extend([response(404, ""), response(200, "")]);
        }
        let (s3, http) = adapter(responses);
        let result = Write::Bytes.run(&s3, policy).await;
        if rename {
            result.unwrap();
            assert!(http
                .requests
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .1
                .contains("/file%20%281%29.txt"));
        } else {
            assert!(matches!(result, Err(BackendError::Conflict { .. })));
            assert_eq!(http.requests.lock().unwrap().len(), 3);
        }
        assert!(http.responses.lock().unwrap().is_empty());
    }
    for write in [Write::Bytes, Write::Reader] {
        let mut responses = Vec::new();
        for _ in 0..=CONDITIONAL_WRITE_RETRIES {
            responses.extend([response(404, ""), response(409, "")]);
        }
        let (s3, http) = adapter(responses);
        assert!(matches!(write.run(&s3, ConflictResolution::Fail).await,
            Err(BackendError::Network(detail)) if detail.message.contains("retry budget exhausted")));
        assert_eq!(
            http.requests.lock().unwrap().len(),
            2 * (CONDITIONAL_WRITE_RETRIES + 1)
        );
        assert!(http.responses.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn copy_rename_retries_412_but_never_409() {
    // Start at an already suffixed destination. Always resolve against the
    // original name, not "file (1) (1).txt", and exclude the known occupied key.
    let (s3, http) = adapter(vec![
        response(200, ""),
        response(404, ""),
        response(412, ""),
        response(200, ""),
        response(404, ""),
        Write::Copy.success(),
    ]);
    Write::Copy
        .run(&s3, ConflictResolution::Rename)
        .await
        .unwrap();
    {
        let requests = http.requests.lock().unwrap();
        assert!(requests[2].1.contains("/file%20%281%29.txt"));
        assert!(requests[5].1.contains("/file%20%282%29.txt"));
        assert_eq!(
            *http.conditions.lock().unwrap(),
            [None, None, Some("*".into()), None, None, Some("*".into())]
        );
        assert!(http.responses.lock().unwrap().is_empty());
    }
    for policy in POLICIES {
        let (s3, http) = adapter(vec![response(404, ""), response(409, "")]);
        assert!(
            matches!(Write::Copy.run(&s3, policy).await, Err(BackendError::Network(detail))
            if detail.message.contains("CopyObject") && detail.message.contains("409"))
        );
        assert_eq!(http.requests.lock().unwrap().len(), 2);
        assert_eq!(http.conditions.lock().unwrap()[1].as_deref(), Some("*"));
    }
}

fn start_upload(id: u8) -> Result<HttpResponse, ConnectorError> {
    response(
        200,
        if id == 1 {
            "<InitiateMultipartUploadResult><UploadId>first</UploadId></InitiateMultipartUploadResult>"
        } else {
            "<InitiateMultipartUploadResult><UploadId>second</UploadId></InitiateMultipartUploadResult>"
        },
    )
}

fn completed_upload() -> Result<HttpResponse, ConnectorError> {
    response(
        200,
        "<CompleteMultipartUploadResult><ETag>test</ETag></CompleteMultipartUploadResult>",
    )
}

#[tokio::test]
async fn buffered_multipart_restarts_all_parts_on_409_or_rename_412() {
    // Two distinct parts prove the complete source, not just its last chunk,
    // is replayed into a fresh upload id.
    let mut data = vec![b'a'; MULTIPART_CHUNK_BYTES];
    data.extend_from_slice(b"last part");
    for status in [409, 412] {
        let (s3, http) = adapter(vec![
            response(404, ""),
            start_upload(1),
            response(200, ""),
            response(200, ""),
            response(status, ""),
            response(204, ""), // Abort failed upload.
            response(404, ""),
            start_upload(2),
            response(200, ""),
            response(200, ""),
            completed_upload(),
        ]);
        s3.write_object(
            &object("file.txt"),
            data.clone(),
            ConflictResolution::Rename,
        )
        .await
        .unwrap();
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 11);
        assert_eq!(requests[5].0, "DELETE");
        assert!(requests[5].1.contains("uploadId=first"));
        for index in [2, 3, 4] {
            assert!(requests[index].1.contains("uploadId=first"));
        }
        for index in [8, 9, 10] {
            assert!(requests[index].1.contains("uploadId=second"));
        }
        assert!(requests[7].1.contains(if status == 412 {
            "/file%20%281%29.txt?"
        } else {
            "/file.txt?"
        }));
        let conditions = http.conditions.lock().unwrap();
        for (index, condition) in conditions.iter().enumerate() {
            assert_eq!(
                condition.as_deref(),
                if [4, 10].contains(&index) {
                    Some("*")
                } else {
                    None
                }
            );
        }
        let bodies = http.bodies.lock().unwrap();
        assert_eq!(bodies[2], body_signature(&data[..MULTIPART_CHUNK_BYTES]));
        assert_eq!(bodies[3], body_signature(&data[MULTIPART_CHUNK_BYTES..]));
        assert_eq!(bodies[8], bodies[2]);
        assert_eq!(bodies[9], bodies[3]);
        assert!(http.responses.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn buffered_multipart_respects_conflict_policy_overwrite_and_retry_budget() {
    // Exercise the multipart policy matrix with a tiny part. The public Vec
    // entry point and its size threshold are exercised by the two-part test.
    let data = vec![1; 3];
    for policy in POLICIES.into_iter().chain([ConflictResolution::Overwrite]) {
        for status in [200, 409, 412] {
            let overwrite = matches!(policy, ConflictResolution::Overwrite);
            let mut responses = Vec::new();
            let retryable = !overwrite
                && (status == 409
                    || (status == 412 && matches!(policy, ConflictResolution::Rename)));
            let attempts = if retryable {
                CONDITIONAL_WRITE_RETRIES + 1
            } else {
                1
            };
            for _ in 0..attempts {
                if !overwrite {
                    responses.push(response(404, ""));
                }
                responses.extend([
                    start_upload(1),
                    response(200, ""),
                    if status == 200 {
                        completed_upload()
                    } else {
                        response(status, "")
                    },
                ]);
                if status != 200 {
                    responses.push(response(204, ""));
                }
            }
            let (mut s3, http) = adapter(responses);
            if overwrite {
                s3.config.custom_endpoint_conditional_writes_confirmed = false;
            }
            let key = s3
                .resolve_write_key("file.txt".into(), &policy)
                .await
                .unwrap();
            let result = s3
                .upload_buffered_multipart("file.txt", key, data.clone(), &policy)
                .await;
            match (status, overwrite) {
                (200, _) => {
                    result.unwrap();
                }
                (412, false) => assert!(matches!(result, Err(BackendError::Conflict { .. }))),
                _ => assert!(matches!(result, Err(BackendError::Network(_)))),
            }
            let requests = http.requests.lock().unwrap();
            assert_eq!(
                requests
                    .iter()
                    .filter(|(_, uri)| uri.contains("uploads"))
                    .count(),
                attempts
            );
            for ((method, uri), condition) in
                requests.iter().zip(http.conditions.lock().unwrap().iter())
            {
                assert_eq!(
                    condition.as_deref(),
                    if !overwrite && method == "POST" && uri.contains("uploadId=") {
                        Some("*")
                    } else {
                        None
                    }
                );
            }
            assert!(http.responses.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn streamed_multipart_409_aborts_without_replaying_consumed_reader() {
    let data = vec![7; SINGLE_PART_UPLOAD_MAX_BYTES + 1];
    for content_length in [None, Some(data.len() as u64)] {
        let (s3, http) = adapter(vec![
            response(404, ""),
            start_upload(1),
            response(200, ""),
            response(409, ""),
            response(204, ""),
        ]);
        assert!(
            matches!(s3.write_from_reader(&object("file.txt"), Box::pin(std::io::Cursor::new(data.clone())), content_length, ConflictResolution::Rename).await,
            Err(BackendError::Network(detail)) if detail.message.contains("new multipart upload") && detail.message.contains("409"))
        );
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 5);
        assert_eq!(requests[4].0, "DELETE");
        assert_eq!(http.conditions.lock().unwrap()[3].as_deref(), Some("*"));
        assert_eq!(http.bodies.lock().unwrap()[2], body_signature(&data));
        assert!(http.responses.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn rename_final_writes_have_a_retry_budget() {
    for write in [Write::Bytes, Write::Reader, Write::Copy] {
        let mut responses = Vec::new();
        for _ in 0..=CONDITIONAL_WRITE_RETRIES {
            responses.extend([response(404, ""), response(412, "")]);
        }
        let (s3, http) = adapter(responses);
        assert!(matches!(
            write.run(&s3, ConflictResolution::Rename).await,
            Err(BackendError::Conflict { .. })
        ));
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 2 * (CONDITIONAL_WRITE_RETRIES + 1));
        let conditions = http.conditions.lock().unwrap();
        for (index, condition) in conditions.iter().enumerate() {
            assert_eq!(
                condition.as_deref(),
                if index % 2 == 1 { Some("*") } else { None }
            );
        }
        assert!(http.responses.lock().unwrap().is_empty());
    }
}
