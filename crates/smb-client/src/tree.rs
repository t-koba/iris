use crate::actor::SmbConnectionActor;
use crate::connection::{SmbConnection, SmbConnectionError};
use crate::packet::tree_connect::{
    Smb2ShareCapabilities, Smb2ShareFlags, Smb2ShareType, Smb2TreeConnectFlags,
    Smb2TreeConnectRequest, Smb2TreeConnectResponse,
};
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SmbTree {
    pub tree_id: u32,
    pub unc_path: String,
    pub share_type: Smb2ShareType,
    pub share_flags: Smb2ShareFlags,
    pub capabilities: Smb2ShareCapabilities,
    pub maximal_access: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum SmbTreeError {
    #[error(transparent)]
    Connection(#[from] SmbConnectionError),

    #[error("SMB tree connect server name is empty")]
    EmptyServer,

    #[error("SMB tree connect share name is empty")]
    EmptyShare,

    #[error("SMB tree connect {component} contains a path separator")]
    PathSeparator { component: &'static str },
}

pub fn unc_share_path(server: &str, share: &str) -> Result<String, SmbTreeError> {
    let server = server.trim();
    let share = share.trim();
    if server.is_empty() {
        return Err(SmbTreeError::EmptyServer);
    }
    if share.is_empty() {
        return Err(SmbTreeError::EmptyShare);
    }
    reject_path_separator("server", server)?;
    reject_path_separator("share", share)?;
    Ok(format!("\\\\{server}\\{share}"))
}

pub async fn tree_connect_share<T>(
    connection: &mut SmbConnection<T>,
    server: &str,
    share: &str,
) -> Result<SmbTree, SmbTreeError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let unc_path = unc_share_path(server, share)?;
    Ok(tree_connect_unc(connection, unc_path).await?)
}

pub async fn tree_connect_share_actor(
    connection: &SmbConnectionActor,
    server: &str,
    share: &str,
) -> Result<SmbTree, SmbTreeError> {
    let unc_path = unc_share_path(server, share)?;
    Ok(tree_connect_unc_actor(connection, unc_path).await?)
}

pub async fn tree_connect_unc<T>(
    connection: &mut SmbConnection<T>,
    unc_path: impl Into<String>,
) -> Result<SmbTree, SmbConnectionError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let unc_path = unc_path.into();
    let response = connection
        .tree_connect(&Smb2TreeConnectRequest {
            flags: Smb2TreeConnectFlags::default(),
            path: unc_path.clone(),
        })
        .await?;
    Ok(SmbTree {
        tree_id: connection.tree_id(),
        unc_path,
        share_type: response.share_type,
        share_flags: response.share_flags,
        capabilities: response.capabilities,
        maximal_access: response.maximal_access,
    })
}

pub async fn tree_connect_unc_actor(
    connection: &SmbConnectionActor,
    unc_path: impl Into<String>,
) -> Result<SmbTree, SmbConnectionError> {
    let unc_path = unc_path.into();
    let request = Smb2TreeConnectRequest {
        flags: Smb2TreeConnectFlags::default(),
        path: unc_path.clone(),
    };
    let response = connection.request_tree_connect(request.encode()?).await?;
    let body = Smb2TreeConnectResponse::decode_packet(&response.to_packet())?;
    Ok(SmbTree {
        tree_id: response.header.tree_id,
        unc_path,
        share_type: body.share_type,
        share_flags: body.share_flags,
        capabilities: body.capabilities,
        maximal_access: body.maximal_access,
    })
}

fn reject_path_separator(component: &'static str, value: &str) -> Result<(), SmbTreeError> {
    if value.contains('\\') || value.contains('/') {
        return Err(SmbTreeError::PathSeparator { component });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::header::{Smb2Command, Smb2Header, SMB2_HEADER_BYTES};
    use crate::packet::tree_connect::{Smb2TreeConnectRequest, Smb2TreeConnectResponse};
    use crate::transport::DirectTcpTransport;
    use tokio::io::duplex;

    #[test]
    fn unc_share_path_formats_and_validates_share_root() {
        assert_eq!(
            unc_share_path(" fileserver ", " data ").expect("unc"),
            "\\\\fileserver\\data"
        );
        assert!(matches!(
            unc_share_path("", "data"),
            Err(SmbTreeError::EmptyServer)
        ));
        assert!(matches!(
            unc_share_path("fileserver", ""),
            Err(SmbTreeError::EmptyShare)
        ));
        assert!(matches!(
            unc_share_path("fileserver", "data/archive"),
            Err(SmbTreeError::PathSeparator { component: "share" })
        ));
    }

    #[tokio::test]
    async fn tree_connect_share_sends_unc_path_and_captures_tree_id() {
        let (client, server) = duplex(1024);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.expect("request");
            let header = Smb2Header::decode(&request).expect("header");
            assert_eq!(header.command, Smb2Command::TreeConnect);
            assert_eq!(header.tree_id, 0);
            let body = &request[SMB2_HEADER_BYTES..];
            let tree = Smb2TreeConnectRequest::decode(body).expect("tree request");
            assert_eq!(tree.path, "\\\\fileserver\\data");

            let mut response_header = Smb2Header::new(Smb2Command::TreeConnect, header.message_id);
            response_header.tree_id = 77;
            transport
                .send_packet(&response_packet(
                    response_header,
                    &tree_connect_response_body(),
                ))
                .await
                .expect("response");
        });

        let mut connection = SmbConnection::new(client);
        connection.set_tree_id(999);
        let tree = tree_connect_share(&mut connection, "fileserver", "data")
            .await
            .expect("tree");

        assert_eq!(tree.tree_id, 77);
        assert_eq!(connection.tree_id(), 77);
        assert_eq!(tree.unc_path, "\\\\fileserver\\data");
        assert_eq!(tree.share_type, Smb2ShareType::Disk);
        assert!(tree.share_flags.contains(Smb2ShareFlags::ENCRYPT_DATA));
        assert!(tree.capabilities.contains(Smb2ShareCapabilities::DFS));
        assert_eq!(tree.maximal_access, 0x001f_01ff);
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn actor_tree_connect_sends_zero_tree_id_and_updates_actor_state() {
        let (client, server) = duplex(1024);
        let actor = SmbConnectionActor::spawn(client);
        actor.set_tree_id(999).await.expect("initial tree");
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.expect("request");
            let header = Smb2Header::decode(&request).expect("header");
            assert_eq!(header.command, Smb2Command::TreeConnect);
            assert_eq!(header.tree_id, 0);
            let body = &request[SMB2_HEADER_BYTES..];
            let tree = Smb2TreeConnectRequest::decode(body).expect("tree request");
            assert_eq!(tree.path, "\\\\fileserver\\data");

            let mut response_header = Smb2Header::new(Smb2Command::TreeConnect, header.message_id);
            response_header.tree_id = 88;
            transport
                .send_packet(&response_packet(
                    response_header,
                    &tree_connect_response_body(),
                ))
                .await
                .expect("response");
        });

        let tree = tree_connect_share_actor(&actor, "fileserver", "data")
            .await
            .expect("tree");

        assert_eq!(tree.tree_id, 88);
        assert_eq!(actor.state().await.expect("state").tree_id, 88);
        server_task.await.expect("server task");
    }

    fn response_packet(header: Smb2Header, body: &[u8]) -> Vec<u8> {
        let mut packet = header.encode().to_vec();
        packet.extend_from_slice(body);
        packet
    }

    fn tree_connect_response_body() -> Vec<u8> {
        let mut body = vec![0; 16];
        put_u16(&mut body, 0, 16);
        body[2] = Smb2ShareType::Disk.to_wire();
        put_u32(&mut body, 4, Smb2ShareFlags::ENCRYPT_DATA.0);
        put_u32(&mut body, 8, Smb2ShareCapabilities::DFS.0);
        put_u32(&mut body, 12, 0x001f_01ff);
        let _ = Smb2TreeConnectResponse::decode_packet(&response_packet(
            Smb2Header::new(Smb2Command::TreeConnect, 0),
            &body,
        ))
        .expect("self-check response body");
        body
    }

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
}
