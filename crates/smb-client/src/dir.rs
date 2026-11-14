use crate::actor::SmbConnectionActor;
use crate::connection::SmbConnectionError;
use crate::file::{close, open, HandleGuard, SmbHandle};
use crate::packet::create::{
    Smb2CreateDisposition, Smb2CreateOptions, Smb2CreateRequest, Smb2DesiredAccess,
    Smb2FileAttributes,
};
use crate::packet::header::{Smb2Command, SmbNtStatus};
use crate::packet::query_directory::{
    Smb2FileInformationClass, Smb2QueryDirectoryFlags, Smb2QueryDirectoryRequest,
    Smb2QueryDirectoryResponse,
};
use crate::wire::{checked_slice, decode_utf16le, le_u32, le_u64};
use crate::SmbClientError;

pub const DEFAULT_DIRECTORY_OUTPUT_BUFFER_LENGTH: u32 = 1024 * 1024;

const FILE_ID_BOTH_DIRECTORY_INFORMATION_FIXED_BYTES: usize = 104;
const FILE_ID_BOTH_DIRECTORY_INFORMATION_SHORT_NAME_OFFSET: usize = 70;
const FILE_ID_BOTH_DIRECTORY_INFORMATION_SHORT_NAME_BYTES: usize = 24;
const FILE_ID_BOTH_DIRECTORY_INFORMATION_FILE_ID_OFFSET: usize = 96;
const FILE_ID_BOTH_DIRECTORY_INFORMATION_FILE_NAME_OFFSET: usize = 104;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SmbDirectoryEntry {
    pub file_index: u32,
    pub creation_time: u64,
    pub last_access_time: u64,
    pub last_write_time: u64,
    pub change_time: u64,
    pub end_of_file: u64,
    pub allocation_size: u64,
    pub file_attributes: Smb2FileAttributes,
    pub ea_size: u32,
    pub short_name: Option<String>,
    pub file_id: u64,
    pub file_name: String,
}

fn directory_request(
    path: impl Into<String>,
    desired_access: u32,
    create_disposition: Smb2CreateDisposition,
) -> Smb2CreateRequest {
    Smb2CreateRequest::new(
        path,
        desired_access,
        Smb2FileAttributes::DIRECTORY,
        create_disposition,
        Smb2CreateOptions::DIRECTORY_FILE,
    )
}

pub async fn create_directory(
    connection: &SmbConnectionActor,
    path: impl Into<String>,
) -> Result<SmbHandle, SmbConnectionError> {
    let request = directory_request(
        path,
        Smb2DesiredAccess::GENERIC_WRITE.0
            | Smb2DesiredAccess::FILE_WRITE_ATTRIBUTES.0
            | Smb2DesiredAccess::SYNCHRONIZE.0,
        Smb2CreateDisposition::Create,
    );
    open(connection, request).await
}

fn query_directory_request(handle: &SmbHandle, first: bool) -> Smb2QueryDirectoryRequest {
    Smb2QueryDirectoryRequest {
        file_information_class: Smb2FileInformationClass::FILE_ID_BOTH_DIRECTORY_INFORMATION,
        flags: if first {
            Smb2QueryDirectoryFlags::RESTART_SCANS
        } else {
            Smb2QueryDirectoryFlags::default()
        },
        file_index: 0,
        file_id: handle.file_id,
        file_name: if first { "*" } else { "" }.to_string(),
        output_buffer_length: DEFAULT_DIRECTORY_OUTPUT_BUFFER_LENGTH,
    }
}

/// Incremental enumeration tied to one actor and server directory handle.
/// Explicitly close abandoned enumerations. Dropping an unclosed enumeration
/// invalidates the transport synchronously, including cancellation during I/O.
pub struct SmbDirectoryEnumeration {
    connection: SmbConnectionActor,
    handle: Option<SmbHandle>,
    pending: std::collections::VecDeque<SmbDirectoryEntry>,
    first: bool,
}

impl Drop for SmbDirectoryEnumeration {
    fn drop(&mut self) {
        if self.handle.is_some() {
            self.connection.shutdown();
        }
    }
}

impl SmbDirectoryEnumeration {
    pub async fn open(
        connection: &SmbConnectionActor,
        path: impl Into<String>,
    ) -> Result<Self, SmbConnectionError> {
        let mut guard = HandleGuard(Some(connection.clone()));
        let request = directory_request(
            path,
            Smb2DesiredAccess::FILE_READ_DATA.0
                | Smb2DesiredAccess::FILE_READ_ATTRIBUTES.0
                | Smb2DesiredAccess::FILE_READ_EA.0
                | Smb2DesiredAccess::SYNCHRONIZE.0,
            Smb2CreateDisposition::Open,
        );
        let handle = match open(connection, request).await {
            Ok(handle) => handle,
            Err(
                error @ SmbConnectionError::UnexpectedStatus {
                    command: Smb2Command::Create,
                    ..
                },
            ) => {
                guard.0 = None; // An explicit CREATE refusal cannot leave a handle.
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        guard.0 = None;
        Ok(Self {
            connection: connection.clone(),
            handle: Some(handle),
            pending: Default::default(),
            first: true,
        })
    }

    pub fn is_closed(&self) -> bool {
        self.handle.is_none()
    }

    /// Return at most `limit` entries, retaining any excess wire response entries.
    /// No look-ahead QUERY_DIRECTORY is issued once the requested page is full.
    /// A full final page may therefore require another call to observe EOF.
    pub async fn next_page(
        &mut self,
        limit: std::num::NonZeroUsize,
    ) -> Result<Vec<SmbDirectoryEntry>, SmbConnectionError> {
        let mut guard = HandleGuard(Some(self.connection.clone()));
        let result = self.read_page(limit.get()).await;
        if result.is_err() {
            let _ = self.close().await;
        }
        guard.0 = None;
        result
    }

    async fn read_page(
        &mut self,
        limit: usize,
    ) -> Result<Vec<SmbDirectoryEntry>, SmbConnectionError> {
        let mut entries = Vec::new();
        while entries.len() < limit {
            if let Some(entry) = self.pending.pop_front() {
                entries.push(entry);
                continue;
            }
            let Some(handle) = self.handle.as_ref() else {
                break;
            };
            let request = query_directory_request(handle, self.first);
            let response = self
                .connection
                .request(Smb2Command::QueryDirectory, request.encode()?)
                .await?;
            if response.status() == SmbNtStatus::NO_MORE_FILES {
                self.close().await?;
                break;
            }
            response.ensure_success(Smb2Command::QueryDirectory)?;
            let page = Smb2QueryDirectoryResponse::decode_packet(&response.to_packet())?;
            if page.output_buffer.is_empty() {
                return Err(SmbClientError::EmptyQueryDirectoryResponse.into());
            }
            self.pending
                .extend(decode_file_id_both_directory_entries(&page.output_buffer)?);
            self.first = false;
        }
        Ok(entries)
    }

    /// Best-effort callers may ignore the result; a failed/cancelled CLOSE leaves
    /// the fallback armed so the generation cannot be reused with a lost handle.
    pub async fn close(&mut self) -> Result<(), SmbConnectionError> {
        if let Some(handle) = self.handle.as_ref() {
            let mut guard = HandleGuard(Some(self.connection.clone()));
            let result = close(&self.connection, handle.clone()).await;
            if result.is_err() {
                self.connection.shutdown();
            }
            self.handle = None;
            self.pending.clear();
            result?;
            guard.0 = None;
        }
        self.pending.clear();
        Ok(())
    }
}

pub async fn list_directory_once(
    connection: &SmbConnectionActor,
    path: impl Into<String>,
) -> Result<Vec<SmbDirectoryEntry>, SmbConnectionError> {
    let mut directory = SmbDirectoryEnumeration::open(connection, path).await?;
    let mut entries = Vec::new();
    while !directory.is_closed() {
        entries.extend(directory.next_page(std::num::NonZeroUsize::MAX).await?);
    }
    Ok(entries)
}

pub fn decode_file_id_both_directory_entries(
    buffer: &[u8],
) -> Result<Vec<SmbDirectoryEntry>, SmbClientError> {
    let mut entries = Vec::new();
    let mut offset = 0usize;
    while offset < buffer.len() {
        let remaining = buffer.len() - offset;
        if remaining < FILE_ID_BOTH_DIRECTORY_INFORMATION_FIXED_BYTES {
            return Err(SmbClientError::BufferRangeOutOfBounds {
                offset,
                length: FILE_ID_BOTH_DIRECTORY_INFORMATION_FIXED_BYTES,
                available: buffer.len(),
            });
        }

        let next_entry_offset = le_u32(buffer, offset) as usize;
        let entry_end = if next_entry_offset == 0 {
            buffer.len()
        } else {
            checked_add_offset(offset, next_entry_offset, buffer.len())?
        };
        let entry = checked_slice(buffer, offset, entry_end - offset)?;
        entries.push(decode_file_id_both_directory_entry(entry)?);

        if next_entry_offset == 0 {
            break;
        }
        offset = entry_end;
    }
    Ok(entries)
}

fn decode_file_id_both_directory_entry(entry: &[u8]) -> Result<SmbDirectoryEntry, SmbClientError> {
    let file_name_length = le_u32(entry, 60) as usize;
    let short_name_length = entry[68] as usize;
    if short_name_length > FILE_ID_BOTH_DIRECTORY_INFORMATION_SHORT_NAME_BYTES {
        return Err(SmbClientError::FieldTooLarge {
            field: "file_id_both_directory.short_name",
            length: short_name_length,
            max: FILE_ID_BOTH_DIRECTORY_INFORMATION_SHORT_NAME_BYTES,
        });
    }
    let short_name_bytes = checked_slice(
        entry,
        FILE_ID_BOTH_DIRECTORY_INFORMATION_SHORT_NAME_OFFSET,
        short_name_length,
    )?;
    let short_name = if short_name_bytes.is_empty() {
        None
    } else {
        Some(decode_utf16le(short_name_bytes)?)
    };
    let file_name = decode_utf16le(checked_slice(
        entry,
        FILE_ID_BOTH_DIRECTORY_INFORMATION_FILE_NAME_OFFSET,
        file_name_length,
    )?)?;

    Ok(SmbDirectoryEntry {
        file_index: le_u32(entry, 4),
        creation_time: le_u64(entry, 8),
        last_access_time: le_u64(entry, 16),
        last_write_time: le_u64(entry, 24),
        change_time: le_u64(entry, 32),
        end_of_file: le_u64(entry, 40),
        allocation_size: le_u64(entry, 48),
        file_attributes: Smb2FileAttributes(le_u32(entry, 56)),
        ea_size: le_u32(entry, 64),
        short_name,
        file_id: le_u64(entry, FILE_ID_BOTH_DIRECTORY_INFORMATION_FILE_ID_OFFSET),
        file_name,
    })
}

fn checked_add_offset(
    offset: usize,
    relative_offset: usize,
    available: usize,
) -> Result<usize, SmbClientError> {
    let Some(end) = offset.checked_add(relative_offset) else {
        return Err(SmbClientError::BufferRangeOutOfBounds {
            offset,
            length: relative_offset,
            available,
        });
    };
    if end > available {
        return Err(SmbClientError::BufferRangeOutOfBounds {
            offset,
            length: relative_offset,
            available,
        });
    }
    Ok(end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    use crate::packet::close::Smb2CloseRequest;
    use crate::packet::create::Smb2FileId;

    use crate::packet::create::Smb2CreateRequest;
    use crate::packet::header::{Smb2Command, Smb2Header, SMB2_HEADER_BYTES};
    use crate::packet::query_directory::Smb2QueryDirectoryRequest;
    use crate::transport::DirectTcpTransport;
    use crate::wire::{align8, encode_utf16le, put_u32, put_u64};
    use tokio::io::duplex;

    #[tokio::test]
    async fn cancelled_enumeration_open_page_and_close_stop_generation() {
        for command in [
            Smb2Command::Create,
            Smb2Command::QueryDirectory,
            Smb2Command::Close,
        ] {
            let (client, server) = duplex(8192);
            let actor = SmbConnectionActor::spawn(client);
            let owned_actor = actor.clone();
            let task = tokio::spawn(async move {
                let mut directory = SmbDirectoryEnumeration::open(&owned_actor, "folder")
                    .await
                    .unwrap();
                if command == Smb2Command::Close {
                    directory.close().await.unwrap();
                } else {
                    directory
                        .next_page(std::num::NonZeroUsize::new(1).unwrap())
                        .await
                        .unwrap();
                }
            });
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.unwrap();
            let header = Smb2Header::decode(&request).unwrap();
            assert_eq!(header.command, Smb2Command::Create);
            if command != Smb2Command::Create {
                transport
                    .send_packet(&response_packet(
                        Smb2Header::new(Smb2Command::Create, header.message_id),
                        &create_response_body(
                            Smb2FileId {
                                persistent: 1,
                                volatile: 2,
                            },
                            Smb2FileAttributes::DIRECTORY,
                            0,
                        ),
                    ))
                    .await
                    .unwrap();
                let request = transport.recv_packet().await.unwrap();
                assert_eq!(Smb2Header::decode(&request).unwrap().command, command);
            }
            // Leave the response outstanding, then cancel the sole state owner.
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            assert!(
                actor.state().await.is_err(),
                "cancelled {command:?} reused its generation"
            );
            assert!(tokio::time::timeout(
                std::time::Duration::from_secs(1),
                transport.recv_packet()
            )
            .await
            .unwrap()
            .is_err());
        }
    }

    #[tokio::test]
    async fn explicit_create_refusal_does_not_stop_the_actor_generation() {
        let (client, server) = duplex(4096);
        let actor = SmbConnectionActor::spawn(client);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.expect("create");
            let header = Smb2Header::decode(&request).expect("header");
            assert_eq!(header.command, Smb2Command::Create);
            let mut response = Smb2Header::new(Smb2Command::Create, header.message_id);
            response.status_or_channel_sequence = 0xc000_0022; // STATUS_ACCESS_DENIED
            transport
                .send_packet(&response_packet(response, &[]))
                .await
                .expect("refusal");
            assert!(transport.recv_packet().await.is_err());
        });

        let error = match SmbDirectoryEnumeration::open(&actor, "folder").await {
            Err(error) => error,
            Ok(_) => panic!("CREATE refusal was accepted"),
        };
        assert!(matches!(
            error,
            SmbConnectionError::UnexpectedStatus {
                command: Smb2Command::Create,
                ..
            }
        ));
        assert!(
            actor.state().await.is_ok(),
            "explicit refusal is not a lost handle"
        );
        actor.shutdown();
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn empty_successful_query_page_fails_closed_and_closes_handle() {
        let (client, server) = duplex(8192);
        let actor = SmbConnectionActor::spawn(client);
        let file_id = Smb2FileId {
            persistent: 3,
            volatile: 4,
        };
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            for expected in [
                Smb2Command::Create,
                Smb2Command::QueryDirectory,
                Smb2Command::Close,
            ] {
                let request = transport.recv_packet().await.expect("request");
                let header = Smb2Header::decode(&request).expect("header");
                assert_eq!(header.command, expected);
                let body = match expected {
                    Smb2Command::Create => {
                        create_response_body(file_id, Smb2FileAttributes::DIRECTORY, 0)
                    }
                    Smb2Command::QueryDirectory => query_directory_response_body(&[]),
                    Smb2Command::Close => {
                        assert_eq!(
                            Smb2CloseRequest::decode(&request[SMB2_HEADER_BYTES..])
                                .expect("close")
                                .file_id,
                            file_id
                        );
                        close_response_body(Smb2FileAttributes::DIRECTORY, 0)
                    }
                    _ => unreachable!(),
                };
                transport
                    .send_packet(&response_packet(
                        Smb2Header::new(expected, header.message_id),
                        &body,
                    ))
                    .await
                    .expect("response");
            }
        });

        let mut directory = SmbDirectoryEnumeration::open(&actor, "folder")
            .await
            .expect("open");
        let error = directory
            .next_page(std::num::NonZeroUsize::new(1).unwrap())
            .await
            .expect_err("empty success is malformed");
        assert!(matches!(
            error,
            SmbConnectionError::Protocol(SmbClientError::EmptyQueryDirectoryResponse)
        ));
        assert!(directory.is_closed());
        assert!(actor.state().await.is_ok());
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn enumeration_error_closes_and_failed_close_stops_generation() {
        for close_fails in [false, true] {
            let (client, server) = duplex(8192);
            let actor = SmbConnectionActor::spawn(client);
            let owned_actor = actor.clone();
            let task = tokio::spawn(async move {
                let mut directory = SmbDirectoryEnumeration::open(&owned_actor, "folder")
                    .await
                    .unwrap();
                assert!(directory
                    .next_page(std::num::NonZeroUsize::new(1).unwrap())
                    .await
                    .is_err());
            });
            let mut transport = DirectTcpTransport::new(server);
            for command in [
                Smb2Command::Create,
                Smb2Command::QueryDirectory,
                Smb2Command::Close,
            ] {
                let request = transport.recv_packet().await.unwrap();
                let header = Smb2Header::decode(&request).unwrap();
                assert_eq!(header.command, command);
                let mut response = Smb2Header::new(command, header.message_id);
                let body = match command {
                    Smb2Command::Create => create_response_body(
                        Smb2FileId {
                            persistent: 1,
                            volatile: 2,
                        },
                        Smb2FileAttributes::DIRECTORY,
                        0,
                    ),
                    Smb2Command::Close if !close_fails => {
                        close_response_body(Smb2FileAttributes::DIRECTORY, 0)
                    }
                    _ => {
                        response.status_or_channel_sequence = 0xc000_0022;
                        Vec::new()
                    }
                };
                transport
                    .send_packet(&response_packet(response, &body))
                    .await
                    .unwrap();
            }
            task.await.unwrap();
            assert_eq!(actor.state().await.is_err(), close_fails);
        }
    }

    #[test]
    fn decode_file_id_both_directory_entries_reads_chained_records() {
        let first = directory_entry_record(1, "alpha.txt", "ALPHA~1.TXT", 42, 0xabc);
        let second = directory_entry_record(2, "folder", "", 0, 0xdef);
        let mut buffer = Vec::new();
        let first_offset = align8(first.len());
        buffer.extend_from_slice(&with_next_offset(first, first_offset as u32));
        buffer.resize(first_offset, 0);
        buffer.extend_from_slice(&second);

        let entries = decode_file_id_both_directory_entries(&buffer).expect("decode");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].file_index, 1);
        assert_eq!(entries[0].file_name, "alpha.txt");
        assert_eq!(entries[0].short_name.as_deref(), Some("ALPHA~1.TXT"));
        assert_eq!(entries[0].end_of_file, 42);
        assert_eq!(entries[0].file_id, 0xabc);
        assert_eq!(entries[1].file_name, "folder");
        assert_eq!(entries[1].short_name, None);
    }

    #[test]
    fn decode_file_id_both_directory_entries_rejects_bad_short_name_length() {
        let mut entry = directory_entry_record(1, "alpha.txt", "", 0, 1);
        entry[68] = 25;

        let error = decode_file_id_both_directory_entries(&entry).expect_err("reject");

        assert_eq!(
            error,
            SmbClientError::FieldTooLarge {
                field: "file_id_both_directory.short_name",
                length: 25,
                max: 24
            }
        );
    }

    #[tokio::test]
    async fn actor_list_directory_once_uses_actor_tree_state() {
        let (client, server) = duplex(4096);
        let actor = SmbConnectionActor::spawn(client);
        actor.set_ids(0, 0x1122_3344).await.expect("ids");
        let file_id = Smb2FileId {
            persistent: 0x0102_0304_0506_0708,
            volatile: 0x1112_1314_1516_1718,
        };
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            for (step, expected) in [
                Smb2Command::Create,
                Smb2Command::QueryDirectory,
                Smb2Command::QueryDirectory,
                Smb2Command::Close,
            ]
            .into_iter()
            .enumerate()
            {
                let request = transport.recv_packet().await.expect("request");
                let header = Smb2Header::decode(&request).expect("header");
                assert_eq!(header.command, expected);
                assert_eq!(header.tree_id, 0x1122_3344);
                let body = &request[SMB2_HEADER_BYTES..];
                let response_body = match expected {
                    Smb2Command::Create => {
                        let create = Smb2CreateRequest::decode(body).expect("create");
                        assert_eq!(create.name, "share/folder");
                        assert!(create
                            .desired_access
                            .contains(Smb2DesiredAccess::FILE_READ_DATA));
                        assert!(create
                            .create_options
                            .contains(Smb2CreateOptions::DIRECTORY_FILE));
                        create_response_body(file_id, Smb2FileAttributes::DIRECTORY, 0)
                    }
                    Smb2Command::QueryDirectory => {
                        let query = Smb2QueryDirectoryRequest::decode(body).expect("query");
                        assert_eq!(
                            query.file_information_class,
                            Smb2FileInformationClass::FILE_ID_BOTH_DIRECTORY_INFORMATION
                        );
                        assert_eq!(
                            query.flags.contains(Smb2QueryDirectoryFlags::RESTART_SCANS),
                            step == 1
                        );
                        assert_eq!(query.file_id, file_id);
                        assert_eq!(query.file_name, if step == 1 { "*" } else { "" });
                        assert_eq!(
                            query.output_buffer_length,
                            DEFAULT_DIRECTORY_OUTPUT_BUFFER_LENGTH
                        );
                        if step == 1 {
                            query_directory_response_body(&directory_entry_record(
                                7,
                                "report.pdf",
                                "",
                                512,
                                0x777,
                            ))
                        } else {
                            Vec::new()
                        }
                    }
                    Smb2Command::Close => {
                        let close = Smb2CloseRequest::decode(body).expect("close");
                        assert_eq!(close.file_id, file_id);
                        close_response_body(Smb2FileAttributes::DIRECTORY, 0)
                    }
                    _ => unreachable!("covered command list"),
                };
                let mut response_header = Smb2Header::new(expected, header.message_id);
                if step == 2 {
                    response_header.status_or_channel_sequence = SmbNtStatus::NO_MORE_FILES.0;
                }
                transport
                    .send_packet(&response_packet(response_header, &response_body))
                    .await
                    .expect("response");
            }
        });

        let entries = list_directory_once(&actor, "share/folder")
            .await
            .expect("list");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].file_index, 7);
        assert_eq!(entries[0].file_name, "report.pdf");
        assert_eq!(entries[0].end_of_file, 512);
        assert_eq!(entries[0].file_id, 0x777);
        server_task.await.expect("server task");
    }

    fn directory_entry_record(
        file_index: u32,
        file_name: &str,
        short_name: &str,
        end_of_file: u64,
        file_id: u64,
    ) -> Vec<u8> {
        let file_name_bytes = encode_utf16le(file_name);
        let short_name_bytes = encode_utf16le(short_name);
        assert!(short_name_bytes.len() <= FILE_ID_BOTH_DIRECTORY_INFORMATION_SHORT_NAME_BYTES);
        let mut body =
            vec![0; FILE_ID_BOTH_DIRECTORY_INFORMATION_FIXED_BYTES + file_name_bytes.len()];
        put_u32(&mut body, 4, file_index);
        put_u64(&mut body, 8, 100);
        put_u64(&mut body, 16, 200);
        put_u64(&mut body, 24, 300);
        put_u64(&mut body, 32, 400);
        put_u64(&mut body, 40, end_of_file);
        put_u64(&mut body, 48, align8(end_of_file as usize) as u64);
        put_u32(&mut body, 56, Smb2FileAttributes::ARCHIVE.0);
        put_u32(&mut body, 60, file_name_bytes.len() as u32);
        put_u32(&mut body, 64, 0);
        body[68] = short_name_bytes.len() as u8;
        body[FILE_ID_BOTH_DIRECTORY_INFORMATION_SHORT_NAME_OFFSET
            ..FILE_ID_BOTH_DIRECTORY_INFORMATION_SHORT_NAME_OFFSET + short_name_bytes.len()]
            .copy_from_slice(&short_name_bytes);
        put_u64(
            &mut body,
            FILE_ID_BOTH_DIRECTORY_INFORMATION_FILE_ID_OFFSET,
            file_id,
        );
        body[FILE_ID_BOTH_DIRECTORY_INFORMATION_FILE_NAME_OFFSET..]
            .copy_from_slice(&file_name_bytes);
        body
    }

    fn with_next_offset(mut entry: Vec<u8>, next_offset: u32) -> Vec<u8> {
        put_u32(&mut entry, 0, next_offset);
        entry
    }
}
