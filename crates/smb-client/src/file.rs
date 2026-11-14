use crate::actor::{SmbCompoundRequest, SmbConnectionActor};
use crate::connection::SmbConnectionError;
use crate::packet::close::{Smb2CloseFlags, Smb2CloseRequest, Smb2CloseResponse};
use crate::packet::create::{
    Smb2CreateDisposition, Smb2CreateOptions, Smb2CreateRequest, Smb2CreateResponse,
    Smb2DesiredAccess, Smb2FileAttributes, Smb2FileId,
};
use crate::packet::header::{smb2_credit_charge_for_payload, Smb2Command};
use crate::packet::read::{Smb2ReadChannel, Smb2ReadFlags, Smb2ReadRequest, Smb2ReadResponse};
use crate::packet::set_info::{
    smb2_file_basic_information, smb2_file_disposition_information, smb2_file_rename_information,
    Smb2FileBasicInformation, Smb2FileInfoClass, Smb2InfoType, Smb2SetInfoRequest,
    Smb2SetInfoResponse,
};
use crate::packet::write::{Smb2WriteChannel, Smb2WriteFlags, Smb2WriteRequest, Smb2WriteResponse};
use tokio::task::JoinHandle;

pub const DEFAULT_READ_PIPELINE_DEPTH: usize = 8;
pub const DEFAULT_READ_CHUNK_LENGTH: u32 = 1024 * 1024;
pub const DEFAULT_WRITE_CHUNK_LENGTH: usize = 1024 * 1024;

/// An open file or directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SmbHandle {
    pub file_id: Smb2FileId,
    pub end_of_file: u64,
    pub allocation_size: u64,
    pub file_attributes: Smb2FileAttributes,
}

impl From<Smb2CreateResponse> for SmbHandle {
    fn from(response: Smb2CreateResponse) -> Self {
        Self {
            file_id: response.file_id,
            end_of_file: response.end_of_file,
            allocation_size: response.allocation_size,
            file_attributes: response.file_attributes,
        }
    }
}

/// The file id that makes a related compound request use the handle opened before it.
const RELATED_FILE_ID: Smb2FileId = Smb2FileId {
    persistent: u64::MAX,
    volatile: u64::MAX,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SmbReadRange {
    pub offset: u64,
    pub length: u32,
}

pub(crate) async fn open(
    connection: &SmbConnectionActor,
    request: Smb2CreateRequest,
) -> Result<SmbHandle, SmbConnectionError> {
    let response = connection
        .request(Smb2Command::Create, request.encode()?)
        .await?;
    response.ensure_success(Smb2Command::Create)?;
    Ok(Smb2CreateResponse::decode_packet(&response.to_packet())?.into())
}

// Armed before CREATE: cancellation can lose even its response. Also used
// around borrowed handle operations so cancelling them cannot resume ambiguously.
pub(crate) struct HandleGuard(pub Option<SmbConnectionActor>);

impl Drop for HandleGuard {
    fn drop(&mut self) {
        if let Some(connection) = self.0.take() {
            connection.shutdown();
        }
    }
}

/// Read metadata for an exact file or directory, without enumerating its parent.
/// Cancellation or an ambiguous CREATE/CLOSE invalidates this actor generation.
pub async fn stat_path(
    connection: &SmbConnectionActor,
    path: impl Into<String>,
) -> Result<Smb2CreateResponse, SmbConnectionError> {
    let mut guard = HandleGuard(Some(connection.clone()));
    let request = Smb2CreateRequest::new(
        path,
        Smb2DesiredAccess::FILE_READ_ATTRIBUTES.0,
        Smb2FileAttributes::default(),
        Smb2CreateDisposition::Open,
        Smb2CreateOptions::default(),
    );
    let response = connection
        .request(Smb2Command::Create, request.encode()?)
        .await?;
    if let Err(error) = response.ensure_success(Smb2Command::Create) {
        guard.0 = None; // The server explicitly refused CREATE: no handle exists.
        return Err(error);
    }
    let metadata = Smb2CreateResponse::decode_packet(&response.to_packet())?;
    close(connection, metadata.clone().into()).await?;
    guard.0 = None;
    Ok(metadata)
}

pub async fn open_file_for_read(
    connection: &SmbConnectionActor,
    path: impl Into<String>,
) -> Result<SmbHandle, SmbConnectionError> {
    open(connection, read_request(path)).await
}

pub async fn open_file_and_read(
    connection: &SmbConnectionActor,
    path: impl Into<String>,
    range: SmbReadRange,
) -> Result<(SmbHandle, Vec<u8>), SmbConnectionError> {
    let (read_body, credit_charge) = read_request_for_range(RELATED_FILE_ID, range)?;
    let responses = connection
        .request_compound(vec![
            SmbCompoundRequest {
                command: Smb2Command::Create,
                body: read_request(path).encode()?,
                credit_charge: 0,
                related: false,
            },
            SmbCompoundRequest {
                command: Smb2Command::Read,
                body: read_body,
                credit_charge,
                related: true,
            },
        ])
        .await?;

    responses[0].ensure_success(Smb2Command::Create)?;
    let create = Smb2CreateResponse::decode_packet(&responses[0].to_packet())?;
    let data = if responses[1].status() == crate::SmbNtStatus::END_OF_FILE {
        Vec::new()
    } else {
        responses[1].ensure_success(Smb2Command::Read)?;
        Smb2ReadResponse::decode_packet(&responses[1].to_packet())?.data
    };
    Ok((create.into(), data))
}

pub async fn open_file_for_write(
    connection: &SmbConnectionActor,
    path: impl Into<String>,
    create_disposition: Smb2CreateDisposition,
) -> Result<SmbHandle, SmbConnectionError> {
    let request = Smb2CreateRequest::new(
        path,
        Smb2DesiredAccess::GENERIC_WRITE.0
            | Smb2DesiredAccess::FILE_WRITE_DATA.0
            | Smb2DesiredAccess::FILE_WRITE_ATTRIBUTES.0
            | Smb2DesiredAccess::SYNCHRONIZE.0,
        Smb2FileAttributes::NORMAL,
        create_disposition,
        Smb2CreateOptions(
            Smb2CreateOptions::NON_DIRECTORY_FILE.0 | Smb2CreateOptions::SEQUENTIAL_ONLY.0,
        ),
    );
    open(connection, request).await
}

/// Attributes and create options that make an open fail unless `path` has the expected kind.
fn kind_options(is_directory: bool) -> (Smb2FileAttributes, Smb2CreateOptions) {
    if is_directory {
        (
            Smb2FileAttributes::DIRECTORY,
            Smb2CreateOptions::DIRECTORY_FILE,
        )
    } else {
        (
            Smb2FileAttributes::NORMAL,
            Smb2CreateOptions::NON_DIRECTORY_FILE,
        )
    }
}

fn delete_access_request(path: impl Into<String>, is_directory: bool) -> Smb2CreateRequest {
    let (attributes, options) = kind_options(is_directory);
    Smb2CreateRequest::new(
        path,
        Smb2DesiredAccess::DELETE.0 | Smb2DesiredAccess::SYNCHRONIZE.0,
        attributes,
        Smb2CreateDisposition::Open,
        options,
    )
}

/// Deletes a file, or an empty directory.
pub async fn delete(
    connection: &SmbConnectionActor,
    path: impl Into<String>,
    is_directory: bool,
) -> Result<(), SmbConnectionError> {
    set_info_by_path(
        connection,
        delete_access_request(path, is_directory),
        Smb2FileInfoClass::FILE_DISPOSITION_INFORMATION,
        smb2_file_disposition_information(true),
    )
    .await
}

pub async fn rename(
    connection: &SmbConnectionActor,
    source_path: impl Into<String>,
    target_path: impl AsRef<str>,
    is_directory: bool,
    replace_if_exists: bool,
) -> Result<(), SmbConnectionError> {
    set_info_by_path(
        connection,
        delete_access_request(source_path, is_directory),
        Smb2FileInfoClass::FILE_RENAME_INFORMATION,
        smb2_file_rename_information(replace_if_exists, target_path.as_ref())?,
    )
    .await
}

/// Sets times and attributes of a file or directory.
pub async fn set_basic_info(
    connection: &SmbConnectionActor,
    path: impl Into<String>,
    info: Smb2FileBasicInformation,
) -> Result<(), SmbConnectionError> {
    let request = Smb2CreateRequest::new(
        path,
        Smb2DesiredAccess::FILE_WRITE_ATTRIBUTES.0 | Smb2DesiredAccess::SYNCHRONIZE.0,
        Smb2FileAttributes::NORMAL,
        Smb2CreateDisposition::Open,
        // Neither DIRECTORY_FILE nor NON_DIRECTORY_FILE: works for files and directories.
        Smb2CreateOptions(0),
    );
    set_info_by_path(
        connection,
        request,
        Smb2FileInfoClass::FILE_BASIC_INFORMATION,
        smb2_file_basic_information(info),
    )
    .await
}

fn read_request(path: impl Into<String>) -> Smb2CreateRequest {
    Smb2CreateRequest::new(
        path,
        Smb2DesiredAccess::GENERIC_READ.0 | Smb2DesiredAccess::SYNCHRONIZE.0,
        Smb2FileAttributes::NORMAL,
        Smb2CreateDisposition::Open,
        Smb2CreateOptions(
            Smb2CreateOptions::NON_DIRECTORY_FILE.0 | Smb2CreateOptions::SEQUENTIAL_ONLY.0,
        ),
    )
}

pub async fn write_file_at(
    connection: &SmbConnectionActor,
    file: &SmbHandle,
    offset: u64,
    data: Vec<u8>,
) -> Result<Smb2WriteResponse, SmbConnectionError> {
    let data_len = checked_write_len(data.len())?;
    let credit_charge = smb2_credit_charge_for_payload(data.len())?;
    let response = connection
        .request_with_credit_charge(
            Smb2Command::Write,
            Smb2WriteRequest {
                offset,
                file_id: file.file_id,
                channel: Smb2WriteChannel::NONE,
                remaining_bytes: 0,
                flags: Smb2WriteFlags::default(),
                data,
                channel_info: Vec::new(),
            }
            .encode()?,
            credit_charge,
        )
        .await?;
    response.ensure_success(Smb2Command::Write)?;
    let write = Smb2WriteResponse::decode_packet(&response.to_packet())?;
    if write.count != data_len {
        return Err(SmbConnectionError::ShortWrite {
            expected: data_len,
            actual: write.count,
        });
    }
    Ok(write)
}

pub async fn write_file_chunks_pipelined(
    connection: &SmbConnectionActor,
    file: &SmbHandle,
    chunks: impl IntoIterator<Item = (u64, Vec<u8>)>,
    max_in_flight: usize,
) -> Result<(), SmbConnectionError> {
    let chunks = chunks.into_iter().collect::<Vec<_>>();
    for window in chunks.chunks(max_in_flight.max(1)) {
        let mut tasks = Vec::with_capacity(window.len());
        for (offset, bytes) in window {
            let actor = connection.clone();
            let file = file.clone();
            let offset = *offset;
            let bytes = bytes.clone();
            tasks.push(tokio::spawn(async move {
                write_file_at(&actor, &file, offset, bytes).await
            }));
        }
        for task in tasks {
            task.await
                .map_err(|error| SmbConnectionError::ActorStopped {
                    reason: error.to_string(),
                })??;
        }
    }
    Ok(())
}

fn read_request_for_range(
    file_id: Smb2FileId,
    range: SmbReadRange,
) -> Result<(Vec<u8>, u16), SmbConnectionError> {
    let credit_charge = smb2_credit_charge_for_payload(range.length as usize)?;
    let request = Smb2ReadRequest {
        flags: Smb2ReadFlags::default(),
        length: range.length,
        offset: range.offset,
        file_id,
        minimum_count: u32::from(range.length > 0),
        channel: Smb2ReadChannel::NONE,
        remaining_bytes: 0,
        channel_info: Vec::new(),
    };
    Ok((request.encode()?, credit_charge))
}

pub async fn read_file_ranges_pipelined(
    connection: &SmbConnectionActor,
    file: &SmbHandle,
    ranges: impl IntoIterator<Item = SmbReadRange>,
    max_in_flight: usize,
) -> Result<Vec<Vec<u8>>, SmbConnectionError> {
    let ranges = ranges.into_iter().collect::<Vec<_>>();
    let mut chunks = Vec::with_capacity(ranges.len());
    for window in ranges.chunks(max_in_flight.max(1)) {
        let mut tasks = Vec::with_capacity(window.len());
        for range in window {
            tasks.push(spawn_read_range(connection.clone(), file.file_id, *range));
        }
        for task in tasks {
            chunks.push(
                task.await
                    .map_err(|error| SmbConnectionError::ActorStopped {
                        reason: error.to_string(),
                    })??,
            );
        }
    }
    Ok(chunks)
}

pub async fn close(
    connection: &SmbConnectionActor,
    handle: SmbHandle,
) -> Result<Smb2CloseResponse, SmbConnectionError> {
    let response = connection
        .request(
            Smb2Command::Close,
            Smb2CloseRequest {
                flags: Smb2CloseFlags::POSTQUERY_ATTRIB,
                file_id: handle.file_id,
            }
            .encode(),
        )
        .await?;
    response.ensure_success(Smb2Command::Close)?;
    Ok(Smb2CloseResponse::decode_packet(&response.to_packet())?)
}

fn spawn_read_range(
    connection: SmbConnectionActor,
    file_id: Smb2FileId,
    range: SmbReadRange,
) -> JoinHandle<Result<Vec<u8>, SmbConnectionError>> {
    tokio::spawn(async move {
        let (request, credit_charge) = read_request_for_range(file_id, range)?;
        let response = connection
            .request_with_credit_charge(Smb2Command::Read, request, credit_charge)
            .await?;
        Ok(Smb2ReadResponse::decode_packet(&response.to_packet())?.data)
    })
}

async fn set_info_by_path(
    connection: &SmbConnectionActor,
    create_request: Smb2CreateRequest,
    file_info_class: Smb2FileInfoClass,
    buffer: Vec<u8>,
) -> Result<(), SmbConnectionError> {
    let responses = connection
        .request_compound(vec![
            SmbCompoundRequest {
                command: Smb2Command::Create,
                body: create_request.encode()?,
                credit_charge: 0,
                related: false,
            },
            SmbCompoundRequest {
                command: Smb2Command::SetInfo,
                body: Smb2SetInfoRequest {
                    info_type: Smb2InfoType::File,
                    file_info_class,
                    additional_information: 0,
                    file_id: RELATED_FILE_ID,
                    buffer,
                }
                .encode()?,
                credit_charge: 0,
                related: true,
            },
            SmbCompoundRequest {
                command: Smb2Command::Close,
                body: Smb2CloseRequest {
                    flags: Smb2CloseFlags::POSTQUERY_ATTRIB,
                    file_id: RELATED_FILE_ID,
                }
                .encode(),
                credit_charge: 0,
                related: true,
            },
        ])
        .await?;

    responses[0].ensure_success(Smb2Command::Create)?;
    let handle = SmbHandle::from(Smb2CreateResponse::decode_packet(
        &responses[0].to_packet(),
    )?);
    let operation_result = responses[1]
        .ensure_success(Smb2Command::SetInfo)
        .and_then(|()| {
            Smb2SetInfoResponse::decode_packet(&responses[1].to_packet())
                .map(|_| ())
                .map_err(Into::into)
        });
    let compound_close_result = responses[2]
        .ensure_success(Smb2Command::Close)
        .and_then(|()| {
            Smb2CloseResponse::decode_packet(&responses[2].to_packet())
                .map(|_| ())
                .map_err(Into::into)
        });

    let close_result = if compound_close_result.is_err() {
        close(connection, handle).await.map(|_| ())
    } else {
        compound_close_result
    };
    operation_result?;
    close_result
}

fn checked_write_len(length: usize) -> Result<u32, SmbConnectionError> {
    u32::try_from(length)
        .map_err(|_| crate::SmbClientError::FieldTooLarge {
            field: "write.data",
            length,
            max: u32::MAX as usize,
        })
        .map_err(SmbConnectionError::Protocol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::SmbConnectionActor;
    use crate::test_support::*;

    use crate::packet::create::Smb2FileAttributes;
    use crate::packet::header::{Smb2Command, Smb2Header, SMB2_HEADER_BYTES};
    use crate::transport::DirectTcpTransport;

    use tokio::io::duplex;

    #[tokio::test]
    async fn actor_open_and_close_file_uses_actor_tree_state() {
        let (client, server) = duplex(4096);
        let actor = SmbConnectionActor::spawn(client);
        actor.set_ids(0, 0x1122_3344).await.expect("ids");
        let file_id = Smb2FileId {
            persistent: 0x0102_0304_0506_0708,
            volatile: 0x1112_1314_1516_1718,
        };
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            for expected in [Smb2Command::Create, Smb2Command::Close] {
                let request = transport.recv_packet().await.expect("request");
                let header = Smb2Header::decode(&request).expect("header");
                assert_eq!(header.command, expected);
                assert_eq!(header.tree_id, 0x1122_3344);
                let body = &request[SMB2_HEADER_BYTES..];
                let response_body = match expected {
                    Smb2Command::Create => {
                        let create = Smb2CreateRequest::decode(body).expect("create");
                        assert_eq!(create.name, "folder/file.bin");
                        create_response_body(file_id, Smb2FileAttributes::ARCHIVE, 7)
                    }
                    Smb2Command::Close => {
                        let close = Smb2CloseRequest::decode(body).expect("close");
                        assert_eq!(close.file_id, file_id);
                        close_response_body(Smb2FileAttributes::ARCHIVE, 7)
                    }
                    _ => unreachable!("covered command list"),
                };
                transport
                    .send_packet(&response_packet(
                        Smb2Header::new(expected, header.message_id),
                        &response_body,
                    ))
                    .await
                    .expect("response");
            }
        });

        let file = open_file_for_read(&actor, "folder/file.bin")
            .await
            .expect("open");
        assert_eq!(file.file_id, file_id);
        let close = close(&actor, file).await.expect("close");
        assert_eq!(close.end_of_file, 7);
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn actor_set_file_basic_info_uses_write_attributes_set_info_and_close() {
        let (client, server) = duplex(4096);
        let actor = SmbConnectionActor::spawn_with_initial_credits(client, 3);
        actor.set_ids(0, 0x1122_3344).await.expect("ids");
        let file_id = Smb2FileId {
            persistent: 0x0102_0304_0506_0708,
            volatile: 0x1112_1314_1516_1718,
        };
        let info = Smb2FileBasicInformation {
            creation_time: 0,
            last_access_time: 0,
            last_write_time: 0x0102_0304_0506_0708,
            change_time: 0,
            file_attributes: Smb2FileAttributes(0),
        };
        let expected_buffer = smb2_file_basic_information(info);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.expect("compound request");
            let create_header = Smb2Header::decode(&request).expect("create header");
            let set_offset = create_header.next_command as usize;
            let set_header = Smb2Header::decode(&request[set_offset..]).expect("set header");
            let close_offset = set_offset + set_header.next_command as usize;
            let close_header = Smb2Header::decode(&request[close_offset..]).expect("close header");
            assert_eq!(create_header.command, Smb2Command::Create);
            assert_eq!(set_header.command, Smb2Command::SetInfo);
            assert_eq!(close_header.command, Smb2Command::Close);
            assert_eq!(create_header.tree_id, 0x1122_3344);

            let create =
                Smb2CreateRequest::decode(&request[SMB2_HEADER_BYTES..set_offset]).expect("create");
            assert_eq!(create.name, "folder/file.bin");
            assert!(create
                .desired_access
                .contains(Smb2DesiredAccess::FILE_WRITE_ATTRIBUTES));
            assert!(!create
                .create_options
                .contains(Smb2CreateOptions::NON_DIRECTORY_FILE));
            assert!(!create
                .create_options
                .contains(Smb2CreateOptions::DIRECTORY_FILE));
            let set_info =
                Smb2SetInfoRequest::decode(&request[set_offset + SMB2_HEADER_BYTES..close_offset])
                    .expect("set info");
            assert_eq!(set_info.info_type, Smb2InfoType::File);
            assert_eq!(
                set_info.file_info_class,
                Smb2FileInfoClass::FILE_BASIC_INFORMATION
            );
            assert_eq!(set_info.file_id.persistent, u64::MAX);
            assert_eq!(set_info.file_id.volatile, u64::MAX);
            assert_eq!(set_info.buffer, expected_buffer);
            let close = Smb2CloseRequest::decode(&request[close_offset + SMB2_HEADER_BYTES..])
                .expect("close");
            assert_eq!(close.file_id.persistent, u64::MAX);
            assert_eq!(close.file_id.volatile, u64::MAX);

            transport
                .send_packet(&compound_response_packet(vec![
                    (
                        Smb2Header::new(Smb2Command::Create, create_header.message_id),
                        create_response_body(file_id, Smb2FileAttributes::ARCHIVE, 7),
                    ),
                    (
                        Smb2Header::new(Smb2Command::SetInfo, set_header.message_id),
                        set_info_response_body(),
                    ),
                    (
                        Smb2Header::new(Smb2Command::Close, close_header.message_id),
                        close_response_body(Smb2FileAttributes::ARCHIVE, 7),
                    ),
                ]))
                .await
                .expect("compound response");
        });

        set_basic_info(&actor, "folder/file.bin", info)
            .await
            .expect("set basic info");
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn pipelined_read_ranges_preserve_requested_order_for_out_of_order_responses() {
        let (client, server) = duplex(4096);
        let actor = SmbConnectionActor::spawn_with_initial_credits(client, 2);
        let file = SmbHandle {
            file_id: Smb2FileId {
                persistent: 0x0102_0304_0506_0708,
                volatile: 0x1112_1314_1516_1718,
            },
            end_of_file: 8,
            allocation_size: 4096,
            file_attributes: Smb2FileAttributes::ARCHIVE,
        };
        let server_file_id = file.file_id;
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let first = transport.recv_packet().await.expect("first read");
            let second = transport.recv_packet().await.expect("second read");
            let first_header = Smb2Header::decode(&first).expect("first header");
            let second_header = Smb2Header::decode(&second).expect("second header");
            let first_read =
                Smb2ReadRequest::decode(&first[SMB2_HEADER_BYTES..]).expect("first body");
            let second_read =
                Smb2ReadRequest::decode(&second[SMB2_HEADER_BYTES..]).expect("second body");
            assert_eq!(first_header.credit_charge, 1);
            assert_eq!(second_header.credit_charge, 1);
            assert_eq!(first_read.file_id, server_file_id);
            assert_eq!(second_read.file_id, server_file_id);
            assert_eq!(first_read.offset, 0);
            assert_eq!(second_read.offset, 4);

            transport
                .send_packet(&response_packet(
                    Smb2Header::new(Smb2Command::Read, second_header.message_id),
                    &read_response_body(b"BBBB"),
                ))
                .await
                .expect("second response");
            transport
                .send_packet(&response_packet(
                    Smb2Header::new(Smb2Command::Read, first_header.message_id),
                    &read_response_body(b"AAAA"),
                ))
                .await
                .expect("first response");
        });

        let chunks = read_file_ranges_pipelined(
            &actor,
            &file,
            [
                SmbReadRange {
                    offset: 0,
                    length: 4,
                },
                SmbReadRange {
                    offset: 4,
                    length: 4,
                },
            ],
            DEFAULT_READ_PIPELINE_DEPTH,
        )
        .await
        .expect("pipeline read");

        assert_eq!(chunks, vec![b"AAAA".to_vec(), b"BBBB".to_vec()]);
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn pipelined_read_ranges_charge_large_reads_by_64k_units() {
        let (client, server) = duplex(4096);
        let actor = SmbConnectionActor::spawn_with_initial_credits(client, 16);
        let file = SmbHandle {
            file_id: Smb2FileId {
                persistent: 0x0102_0304_0506_0708,
                volatile: 0x1112_1314_1516_1718,
            },
            end_of_file: u64::from(DEFAULT_READ_CHUNK_LENGTH),
            allocation_size: u64::from(DEFAULT_READ_CHUNK_LENGTH),
            file_attributes: Smb2FileAttributes::ARCHIVE,
        };
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.expect("read");
            let header = Smb2Header::decode(&request).expect("header");
            let read = Smb2ReadRequest::decode(&request[SMB2_HEADER_BYTES..]).expect("body");
            assert_eq!(header.credit_charge, 16);
            assert_eq!(read.length, DEFAULT_READ_CHUNK_LENGTH);

            transport
                .send_packet(&response_packet(
                    Smb2Header::new(Smb2Command::Read, header.message_id),
                    &read_response_body(b"chunk"),
                ))
                .await
                .expect("response");
        });

        let chunks = read_file_ranges_pipelined(
            &actor,
            &file,
            [SmbReadRange {
                offset: 0,
                length: DEFAULT_READ_CHUNK_LENGTH,
            }],
            DEFAULT_READ_PIPELINE_DEPTH,
        )
        .await
        .expect("pipeline read");

        assert_eq!(chunks, vec![b"chunk".to_vec()]);
        assert_eq!(
            actor.state().await.expect("actor state").next_message_id,
            16
        );
        server_task.await.expect("server task");
    }
}
