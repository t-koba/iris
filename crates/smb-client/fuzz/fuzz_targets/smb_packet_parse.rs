#![no_main]

use iris_smb_client::{decode_compound_responses, decode_file_id_both_directory_entries};
use iris_smb_client::packet::{
    close::{Smb2CloseRequest, Smb2CloseResponse},
    create::{Smb2CreateRequest, Smb2CreateResponse},
    header::{Smb2Command, Smb2Header, SMB2_HEADER_BYTES},
    ioctl::{Smb2IoctlRequest, Smb2IoctlResponse},
    query_directory::{Smb2QueryDirectoryRequest, Smb2QueryDirectoryResponse},
    read::{Smb2ReadRequest, Smb2ReadResponse},
    session_setup::{Smb2SessionSetupRequest, Smb2SessionSetupResponse},
    set_info::{Smb2SetInfoRequest, Smb2SetInfoResponse},
    tree_connect::{Smb2TreeConnectRequest, Smb2TreeConnectResponse},
    write::{Smb2WriteRequest, Smb2WriteResponse},
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = decode_file_id_both_directory_entries(data);
    let _ = decode_compound_responses(data);
    let Ok(header) = Smb2Header::decode(data) else {
        return;
    };
    let reparsed = Smb2Header::decode(&header.encode()).expect("encoded header must parse");
    assert_eq!(reparsed, header);

    let body = &data[SMB2_HEADER_BYTES..];
    match header.command {
        Smb2Command::SessionSetup => {
            let _ = Smb2SessionSetupRequest::decode(body);
            let _ = Smb2SessionSetupResponse::decode_packet(data);
        }
        Smb2Command::TreeConnect => {
            let _ = Smb2TreeConnectRequest::decode(body);
            let _ = Smb2TreeConnectResponse::decode_packet(data);
        }
        Smb2Command::Create => {
            let _ = Smb2CreateRequest::decode(body);
            let _ = Smb2CreateResponse::decode_packet(data);
        }
        Smb2Command::Close => {
            let _ = Smb2CloseRequest::decode(body);
            let _ = Smb2CloseResponse::decode_packet(data);
        }
        Smb2Command::Read => {
            let _ = Smb2ReadRequest::decode(body);
            let _ = Smb2ReadResponse::decode_packet(data);
        }
        Smb2Command::Write => {
            let _ = Smb2WriteRequest::decode(body);
            let _ = Smb2WriteResponse::decode_packet(data);
        }
        Smb2Command::Ioctl => {
            let _ = Smb2IoctlRequest::decode(body);
            let _ = Smb2IoctlResponse::decode_packet(data);
        }
        Smb2Command::QueryDirectory => {
            let _ = Smb2QueryDirectoryRequest::decode(body);
            let _ = Smb2QueryDirectoryResponse::decode_packet(data);
        }
        Smb2Command::SetInfo => {
            let _ = Smb2SetInfoRequest::decode(body);
            let _ = Smb2SetInfoResponse::decode_packet(data);
        }
        Smb2Command::Negotiate
        | Smb2Command::Logoff
        | Smb2Command::TreeDisconnect
        | Smb2Command::Flush
        | Smb2Command::Lock
        | Smb2Command::Cancel
        | Smb2Command::Echo
        | Smb2Command::ChangeNotify
        | Smb2Command::QueryInfo
        | Smb2Command::OplockBreak => {}
    }
});
