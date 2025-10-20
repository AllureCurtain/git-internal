use std::pin::Pin;

use bytes::{BufMut, Bytes, BytesMut};
use futures::Stream;
use std::collections::HashMap;
use tokio_stream::wrappers::ReceiverStream;

use super::core::{AuthenticationService, RepositoryAccess};
use super::pack::PackGenerator;
use super::types::ProtocolError;
use super::types::{
    COMMON_CAP_LIST, Capability, LF, NUL, PKT_LINE_END_MARKER, RECEIVE_CAP_LIST, RefCommand,
    RefTypeEnum, SP, ServiceType, SideBind, TransportProtocol, UPLOAD_CAP_LIST, ZERO_ID,
};
use super::utils::{add_pkt_line_string, build_smart_reply, read_pkt_line, read_until_white_space};

/// Smart Git Protocol implementation
///
/// This struct handles the Git smart protocol operations for both HTTP and SSH transports.
/// It uses trait abstractions to decouple from specific business logic implementations.
pub struct SmartProtocol<R, A>
where
    R: RepositoryAccess,
    A: AuthenticationService,
{
    pub transport_protocol: TransportProtocol,
    pub service_type: Option<ServiceType>,
    pub capabilities: Vec<Capability>,
    pub side_bind: Option<SideBind>,
    pub command_list: Vec<RefCommand>,

    // Trait-based dependencies
    repo_storage: R,
    auth_service: A,
}

impl<R, A> SmartProtocol<R, A>
where
    R: RepositoryAccess,
    A: AuthenticationService,
{
    /// Create a new SmartProtocol instance
    pub fn new(transport_protocol: TransportProtocol, repo_storage: R, auth_service: A) -> Self {
        Self {
            transport_protocol,
            service_type: None,
            capabilities: Vec::new(),
            side_bind: None,
            command_list: Vec::new(),
            repo_storage,
            auth_service,
        }
    }

    /// Authenticate an HTTP request using the injected auth service
    pub async fn authenticate_http(
        &self,
        headers: &HashMap<String, String>,
    ) -> Result<(), ProtocolError> {
        self.auth_service.authenticate_http(headers).await
    }

    /// Authenticate an SSH session using username and public key
    pub async fn authenticate_ssh(
        &self,
        username: &str,
        public_key: &[u8],
    ) -> Result<(), ProtocolError> {
        self.auth_service
            .authenticate_ssh(username, public_key)
            .await
    }

    /// Set the service type for this protocol session
    pub fn set_service_type(&mut self, service_type: ServiceType) {
        self.service_type = Some(service_type);
    }

    /// Get the current service type
    pub fn get_service_type(&self) -> Option<ServiceType> {
        self.service_type
    }

    /// Set transport protocol (Http, Ssh, etc.)
    pub fn set_transport_protocol(&mut self, protocol: TransportProtocol) {
        self.transport_protocol = protocol;
    }

    /// Get git info refs for the repository
    pub async fn git_info_refs(&self, repo_path: &str) -> Result<BytesMut, ProtocolError> {
        let service_type = self
            .service_type
            .ok_or_else(|| ProtocolError::repository_error("Service type not set".to_string()))?;

        let refs = self
            .repo_storage
            .get_repository_refs(repo_path)
            .await
            .map_err(|e| ProtocolError::repository_error(format!("Failed to get refs: {}", e)))?;

        // Convert to the expected format (head_hash, git_refs)
        let head_hash = refs
            .iter()
            .find(|(name, _)| {
                name == "HEAD" || name.ends_with("/main") || name.ends_with("/master")
            })
            .map(|(_, hash)| hash.clone())
            .unwrap_or_else(|| "0000000000000000000000000000000000000000".to_string());

        let git_refs: Vec<super::types::GitRef> = refs
            .into_iter()
            .map(|(name, hash)| super::types::GitRef { name, hash })
            .collect();

        // Determine capabilities based on service type
        let cap_list = match service_type {
            ServiceType::UploadPack => format!("{UPLOAD_CAP_LIST}{COMMON_CAP_LIST}"),
            ServiceType::ReceivePack => format!("{RECEIVE_CAP_LIST}{COMMON_CAP_LIST}"),
        };

        // The stream MUST include capability declarations behind a NUL on the first ref.
        let name = if head_hash == ZERO_ID {
            "capabilities^{}"
        } else {
            "HEAD"
        };
        let pkt_line = format!("{head_hash}{SP}{name}{NUL}{cap_list}{LF}");
        let mut ref_list = vec![pkt_line];

        for git_ref in git_refs {
            let pkt_line = format!("{}{}{}{}", git_ref.hash, SP, git_ref.name, LF);
            ref_list.push(pkt_line);
        }

        let pkt_line_stream =
            build_smart_reply(self.transport_protocol, &ref_list, service_type.to_string());
        tracing::debug!("git_info_refs, return: --------> {:?}", pkt_line_stream);
        Ok(pkt_line_stream)
    }

    /// Handle git upload-pack operation (fetch/clone)
    pub async fn git_upload_pack(
        &mut self,
        repo_path: &str,
        upload_request: &mut Bytes,
    ) -> Result<(ReceiverStream<Vec<u8>>, BytesMut), ProtocolError> {
        let mut want: Vec<String> = Vec::new();
        let mut have: Vec<String> = Vec::new();
        let mut last_common_commit = String::new();

        let mut read_first_line = false;
        loop {
            let (bytes_take, pkt_line) = read_pkt_line(upload_request);

            if bytes_take == 0 {
                break;
            }

            if pkt_line.is_empty() {
                break;
            }

            let mut pkt_line = pkt_line;
            let command = read_until_white_space(&mut pkt_line);

            match command.as_str() {
                "want" => {
                    let hash = read_until_white_space(&mut pkt_line);
                    want.push(hash);
                    if !read_first_line {
                        let cap_str = String::from_utf8_lossy(&pkt_line).to_string();
                        self.parse_capabilities(&cap_str);
                        read_first_line = true;
                    }
                }
                "have" => {
                    let hash = read_until_white_space(&mut pkt_line);
                    have.push(hash);
                }
                "done" => {
                    break;
                }
                _ => {
                    tracing::warn!("Unknown upload-pack command: {}", command);
                }
            }
        }

        let mut protocol_buf = BytesMut::new();

        // Create pack generator for this operation
        let pack_generator = PackGenerator::new(&self.repo_storage, repo_path);

        if have.is_empty() {
            // Full pack
            add_pkt_line_string(&mut protocol_buf, String::from("NAK\n"));
            let pack_stream = pack_generator.generate_full_pack(want).await?;
            return Ok((pack_stream, protocol_buf));
        }

        // Check for common commits
        for hash in &have {
            let exists = self
                .repo_storage
                .commit_exists(repo_path, hash)
                .await
                .map_err(|e| {
                    ProtocolError::repository_error(format!(
                        "Failed to check commit existence: {}",
                        e
                    ))
                })?;
            if exists {
                add_pkt_line_string(&mut protocol_buf, format!("ACK {hash} common\n"));
                if last_common_commit.is_empty() {
                    last_common_commit = hash.clone();
                }
            }
        }

        if last_common_commit.is_empty() {
            // No common commits found
            add_pkt_line_string(&mut protocol_buf, String::from("NAK\n"));
            let pack_stream = pack_generator.generate_full_pack(want).await?;
            return Ok((pack_stream, protocol_buf));
        }

        // Generate incremental pack
        add_pkt_line_string(
            &mut protocol_buf,
            format!("ACK {last_common_commit} ready\n"),
        );
        protocol_buf.put(&PKT_LINE_END_MARKER[..]);

        add_pkt_line_string(&mut protocol_buf, format!("ACK {last_common_commit} \n"));

        let pack_stream = pack_generator.generate_incremental_pack(want, have).await?;

        Ok((pack_stream, protocol_buf))
    }

    /// Parse receive pack commands from protocol bytes
    pub fn parse_receive_pack_commands(&mut self, mut protocol_bytes: Bytes) {
        loop {
            let (bytes_take, pkt_line) = read_pkt_line(&mut protocol_bytes);

            if bytes_take == 0 {
                break;
            }

            if pkt_line.is_empty() {
                break;
            }

            let ref_command = self.parse_ref_command(&mut pkt_line.clone());
            self.command_list.push(ref_command);
        }
    }

    /// Handle git receive-pack operation (push)
    pub async fn git_receive_pack_stream(
        &mut self,
        repo_path: &str,
        data_stream: Pin<Box<dyn Stream<Item = Result<Bytes, ProtocolError>> + Send>>,
    ) -> Result<Bytes, ProtocolError> {
        // Collect all pack data from stream
        let mut pack_data = BytesMut::new();
        let mut stream = data_stream;

        while let Some(chunk_result) = futures::StreamExt::next(&mut stream).await {
            let chunk = chunk_result
                .map_err(|e| ProtocolError::invalid_request(&format!("Stream error: {}", e)))?;
            pack_data.extend_from_slice(&chunk);
        }

        // Create pack generator for unpacking
        let pack_generator = PackGenerator::new(&self.repo_storage, repo_path);

        // Unpack the received data
        let (commits, trees, blobs) = pack_generator.unpack_stream(pack_data.freeze()).await?;

        // Store the unpacked objects via the repository access trait
        self.repo_storage
            .handle_pack_objects(repo_path, commits, trees, blobs)
            .await
            .map_err(|e| {
                ProtocolError::repository_error(format!("Failed to store pack objects: {}", e))
            })?;

        // Build status report
        let mut report_status = BytesMut::new();
        add_pkt_line_string(&mut report_status, "unpack ok\n".to_owned());

        let mut default_exist = self
            .repo_storage
            .has_default_branch(repo_path)
            .await
            .map_err(|e| {
                ProtocolError::repository_error(format!("Failed to check default branch: {}", e))
            })?;

        // Update refs with proper error handling
        for command in &mut self.command_list {
            if command.ref_type == RefTypeEnum::Tag {
                // Just update if refs type is tag
                // Convert ZERO_ID to None for old hash
                let old_hash = if command.old_hash == ZERO_ID {
                    None
                } else {
                    Some(command.old_hash.as_str())
                };
                if let Err(e) = self
                    .repo_storage
                    .update_reference(repo_path, &command.ref_name, old_hash, &command.new_hash)
                    .await
                {
                    command.failed(e.to_string());
                }
            } else {
                // Handle default branch setting for the first branch
                if !default_exist {
                    command.default_branch = true;
                    default_exist = true;
                }
                // Convert ZERO_ID to None for old hash
                let old_hash = if command.old_hash == ZERO_ID {
                    None
                } else {
                    Some(command.old_hash.as_str())
                };
                if let Err(e) = self
                    .repo_storage
                    .update_reference(repo_path, &command.ref_name, old_hash, &command.new_hash)
                    .await
                {
                    command.failed(e.to_string());
                }
            }
            add_pkt_line_string(&mut report_status, command.get_status());
        }

        // Post-receive hook
        self.repo_storage
            .post_receive_hook(repo_path)
            .await
            .map_err(|e| {
                ProtocolError::repository_error(format!("Post-receive hook failed: {}", e))
            })?;

        report_status.put(&PKT_LINE_END_MARKER[..]);
        Ok(report_status.freeze())
    }

    /// Builds the packet data in the sideband format if the SideBand/64k capability is enabled.
    pub fn build_side_band_format(&self, from_bytes: BytesMut, length: usize) -> BytesMut {
        let mut to_bytes = BytesMut::new();
        if self.capabilities.contains(&Capability::SideBand)
            || self.capabilities.contains(&Capability::SideBand64k)
        {
            let length = length + 5;
            to_bytes.put(Bytes::from(format!("{length:04x}")));
            to_bytes.put_u8(SideBind::PackfileData.value());
            to_bytes.put(from_bytes);
        } else {
            to_bytes.put(from_bytes);
        }
        to_bytes
    }

    /// Parse capabilities from capability string
    pub fn parse_capabilities(&mut self, cap_str: &str) {
        for cap in cap_str.split_whitespace() {
            if let Ok(capability) = cap.parse::<Capability>() {
                self.capabilities.push(capability);
            }
        }
    }

    /// Parse a reference command from packet line
    pub fn parse_ref_command(&self, pkt_line: &mut Bytes) -> RefCommand {
        let old_id = read_until_white_space(pkt_line);
        let new_id = read_until_white_space(pkt_line);
        let ref_name = read_until_white_space(pkt_line);
        let _capabilities = String::from_utf8_lossy(&pkt_line[..]).to_string();

        RefCommand::new(old_id, new_id, ref_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::object::blob::Blob;
    use crate::internal::object::commit::Commit;
    use crate::internal::object::signature::{Signature, SignatureType};
    use crate::internal::object::tree::{Tree, TreeItem, TreeItemMode};
    use crate::internal::pack::{encode::PackEncoder, entry::Entry};
    use crate::protocol::types::{RefCommand, ZERO_ID}; // import sibling types
    use crate::protocol::utils; // import sibling module
    use async_trait::async_trait;
    use bytes::Bytes;
    use futures;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use tokio::sync::mpsc;

    // Simplify complex type via aliases to satisfy clippy::type_complexity
    type UpdateRecord = (String, Option<String>, String);
    type UpdateList = Vec<UpdateRecord>;
    type SharedUpdates = Arc<Mutex<UpdateList>>;

    #[derive(Clone)]
    struct TestRepoAccess {
        updates: SharedUpdates,
        stored_count: Arc<Mutex<usize>>,
        default_branch_exists: Arc<Mutex<bool>>,
        post_called: Arc<AtomicBool>,
    }

    impl TestRepoAccess {
        fn new() -> Self {
            Self {
                updates: Arc::new(Mutex::new(vec![])),
                stored_count: Arc::new(Mutex::new(0)),
                default_branch_exists: Arc::new(Mutex::new(false)),
                post_called: Arc::new(AtomicBool::new(false)),
            }
        }

        fn updates_len(&self) -> usize {
            self.updates.lock().unwrap().len()
        }

        fn post_hook_called(&self) -> bool {
            self.post_called.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl RepositoryAccess for TestRepoAccess {
        async fn get_repository_refs(
            &self,
            _repo_path: &str,
        ) -> Result<Vec<(String, String)>, ProtocolError> {
            Ok(vec![
                (
                    "HEAD".to_string(),
                    "0000000000000000000000000000000000000000".to_string(),
                ),
                (
                    "refs/heads/main".to_string(),
                    "1111111111111111111111111111111111111111".to_string(),
                ),
            ])
        }

        async fn has_object(
            &self,
            _repo_path: &str,
            _object_hash: &str,
        ) -> Result<bool, ProtocolError> {
            Ok(true)
        }

        async fn get_object(
            &self,
            _repo_path: &str,
            _object_hash: &str,
        ) -> Result<Vec<u8>, ProtocolError> {
            Ok(vec![])
        }

        async fn store_pack_data(
            &self,
            _repo_path: &str,
            _pack_data: &[u8],
        ) -> Result<(), ProtocolError> {
            *self.stored_count.lock().unwrap() += 1;
            Ok(())
        }

        async fn update_reference(
            &self,
            _repo_path: &str,
            ref_name: &str,
            old_hash: Option<&str>,
            new_hash: &str,
        ) -> Result<(), ProtocolError> {
            self.updates.lock().unwrap().push((
                ref_name.to_string(),
                old_hash.map(|s| s.to_string()),
                new_hash.to_string(),
            ));
            Ok(())
        }

        async fn get_objects_for_pack(
            &self,
            _repo_path: &str,
            _wants: &[String],
            _haves: &[String],
        ) -> Result<Vec<String>, ProtocolError> {
            Ok(vec![])
        }

        async fn has_default_branch(&self, _repo_path: &str) -> Result<bool, ProtocolError> {
            let mut exists = self.default_branch_exists.lock().unwrap();
            let current = *exists;
            *exists = true; // flip to true after first check
            Ok(current)
        }

        async fn post_receive_hook(&self, _repo_path: &str) -> Result<(), ProtocolError> {
            self.post_called.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    struct TestAuth;

    #[async_trait]
    impl AuthenticationService for TestAuth {
        async fn authenticate_http(
            &self,
            _headers: &std::collections::HashMap<String, String>,
        ) -> Result<(), ProtocolError> {
            Ok(())
        }

        async fn authenticate_ssh(
            &self,
            _username: &str,
            _public_key: &[u8],
        ) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_receive_pack_stream_status_report() {
        // Build simple objects
        let blob1 = Blob::from_content("hello");
        let blob2 = Blob::from_content("world");

        let item1 = TreeItem::new(TreeItemMode::Blob, blob1.id, "hello.txt".to_string());
        let item2 = TreeItem::new(TreeItemMode::Blob, blob2.id, "world.txt".to_string());
        let tree = Tree::from_tree_items(vec![item1, item2]).unwrap();

        let author = Signature::new(
            SignatureType::Author,
            "tester".to_string(),
            "tester@example.com".to_string(),
        );
        let committer = Signature::new(
            SignatureType::Committer,
            "tester".to_string(),
            "tester@example.com".to_string(),
        );
        let commit = Commit::new(author, committer, tree.id, vec![], "init commit");

        // Encode pack bytes via PackEncoder
        let (pack_tx, mut pack_rx) = mpsc::channel(1024);
        let (entry_tx, entry_rx) = mpsc::channel(1024);
        let mut encoder = PackEncoder::new(4, 10, pack_tx);

        tokio::spawn(async move {
            if let Err(e) = encoder.encode(entry_rx).await {
                panic!("Failed to encode pack: {}", e);
            }
        });

        let commit_clone = commit.clone();
        let tree_clone = tree.clone();
        let blob1_clone = blob1.clone();
        let blob2_clone = blob2.clone();
        tokio::spawn(async move {
            let _ = entry_tx.send(Entry::from(commit_clone)).await;
            let _ = entry_tx.send(Entry::from(tree_clone)).await;
            let _ = entry_tx.send(Entry::from(blob1_clone)).await;
            let _ = entry_tx.send(Entry::from(blob2_clone)).await;
            // sender drop indicates end
        });

        let mut pack_bytes: Vec<u8> = Vec::new();
        while let Some(chunk) = pack_rx.recv().await {
            pack_bytes.extend_from_slice(&chunk);
        }

        // Prepare protocol and command
        let repo_access = TestRepoAccess::new();
        let auth = TestAuth;
        let mut smart = SmartProtocol::new(TransportProtocol::Http, repo_access.clone(), auth);
        smart.command_list.push(RefCommand::new(
            ZERO_ID.to_string(),
            commit.id.to_string(),
            "refs/heads/main".to_string(),
        ));

        // Create request stream
        let request_stream = Box::pin(futures::stream::once(async { Ok(Bytes::from(pack_bytes)) }));

        // Execute receive-pack
        let result_bytes = smart
            .git_receive_pack_stream("test-repo-path", request_stream)
            .await
            .expect("receive-pack should succeed");

        // Verify pkt-lines
        let mut out = result_bytes.clone();
        let (_c1, l1) = utils::read_pkt_line(&mut out);
        assert_eq!(String::from_utf8(l1.to_vec()).unwrap(), "unpack ok\n");

        let (_c2, l2) = utils::read_pkt_line(&mut out);
        assert_eq!(
            String::from_utf8(l2.to_vec()).unwrap(),
            "ok refs/heads/main"
        );

        let (c3, l3) = utils::read_pkt_line(&mut out);
        assert_eq!(c3, 4);
        assert!(l3.is_empty());

        // Verify side effects
        assert_eq!(repo_access.updates_len(), 1);
        assert!(repo_access.post_hook_called());
    }
}
