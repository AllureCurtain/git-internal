use bytes::Bytes;
use futures::stream::Stream;
use std::fmt;
use std::pin::Pin;
use std::str::FromStr;

/// Type alias for protocol data streams to reduce nesting
pub type ProtocolStream = Pin<Box<dyn Stream<Item = Result<Bytes, ProtocolError>> + Send>>;

/// Protocol error types
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("Invalid service: {0}")]
    InvalidService(String),

    #[error("Repository not found: {0}")]
    RepositoryNotFound(String),

    #[error("Object not found: {0}")]
    ObjectNotFound(String),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Pack error: {0}")]
    Pack(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl ProtocolError {
    pub fn invalid_service(service: &str) -> Self {
        ProtocolError::InvalidService(service.to_string())
    }

    pub fn repository_error(msg: String) -> Self {
        ProtocolError::Internal(msg)
    }

    pub fn invalid_request(msg: &str) -> Self {
        ProtocolError::InvalidRequest(msg.to_string())
    }

    pub fn unauthorized(msg: &str) -> Self {
        ProtocolError::Unauthorized(msg.to_string())
    }
}

/// Git transport protocol types
#[derive(Debug, PartialEq, Clone, Copy, Default)]
pub enum TransportProtocol {
    Local,
    #[default]
    Http,
    Ssh,
    Git,
}

/// Git service types for smart protocol
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum ServiceType {
    UploadPack,
    ReceivePack,
}

impl fmt::Display for ServiceType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ServiceType::UploadPack => write!(f, "git-upload-pack"),
            ServiceType::ReceivePack => write!(f, "git-receive-pack"),
        }
    }
}

impl FromStr for ServiceType {
    type Err = ProtocolError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "git-upload-pack" => Ok(ServiceType::UploadPack),
            "git-receive-pack" => Ok(ServiceType::ReceivePack),
            _ => Err(ProtocolError::InvalidService(s.to_string())),
        }
    }
}

/// Git protocol capabilities
///
/// ## Implementation Status Overview
///
/// ### Implemented capabilities:
/// - **Data transmission**: SideBand, SideBand64k - Multiplexed data streams via side-band formatter
/// - **Status reporting**: ReportStatus, ReportStatusv2 - Push status feedback via protocol handlers
/// - **Pack optimization**: OfsDelta, ThinPack, NoThin - Delta compression and efficient transmission
/// - **Protocol control**: MultiAckDetailed, NoDone - ACK mechanism optimization for upload-pack
/// - **Push control**: Atomic, DeleteRefs, Quiet - Atomic operations and reference management
/// - **Tag handling**: IncludeTag - Automatic tag inclusion for upload-pack
/// - **Client identification**: Agent - Client/server identification in capability negotiation
///
/// ### Not yet implemented capabilities:
/// - **Basic protocol**: MultiAck - Basic multi-ack support (only detailed version implemented)
/// - **Shallow cloning**: Shallow, DeepenSince, DeepenNot, DeepenRelative - Depth control for shallow clones
/// - **Progress control**: NoProgress - Progress output suppression
/// - **Special fetch**: AllowTipSha1InWant, AllowReachableSha1InWant - SHA1 validation in want processing
/// - **Security**: PushCert - Push certificate verification mechanism
/// - **Extensions**: PushOptions, Filter, Symref - Extended parameter handling
/// - **Session management**: SessionId, ObjectFormat - Session and format negotiation
#[derive(Debug, Clone, PartialEq)]
pub enum Capability {
    /// Multi-ack capability for upload-pack protocol
    MultiAck,
    /// Multi-ack-detailed capability for more granular acknowledgment
    MultiAckDetailed,
    /// No-done capability to optimize upload-pack protocol
    NoDone,
    /// Side-band capability for multiplexing data streams
    SideBand,
    /// Side-band-64k capability for larger side-band packets
    SideBand64k,
    /// Report-status capability for push status reporting
    ReportStatus,
    /// Report-status-v2 capability for enhanced push status reporting
    ReportStatusv2,
    /// OFS-delta capability for offset-based delta compression
    OfsDelta,
    /// Deepen-since capability for shallow clone with time-based depth
    DeepenSince,
    /// Deepen-not capability for shallow clone exclusions
    DeepenNot,
    /// Deepen-relative capability for relative depth specification
    DeepenRelative,
    /// Thin-pack capability for efficient pack transmission
    ThinPack,
    /// Shallow capability for shallow clone support
    Shallow,
    /// Include-tag capability for automatic tag inclusion
    IncludeTag,
    /// Delete-refs capability for reference deletion
    DeleteRefs,
    /// Quiet capability to suppress output
    Quiet,
    /// Atomic capability for atomic push operations
    Atomic,
    /// No-thin capability to disable thin pack
    NoThin,
    /// No-progress capability to disable progress reporting
    NoProgress,
    /// Allow-tip-sha1-in-want capability for fetching specific commits
    AllowTipSha1InWant,
    /// Allow-reachable-sha1-in-want capability for fetching reachable commits
    AllowReachableSha1InWant,
    /// Push-cert capability for signed push certificates
    PushCert(String),
    /// Push-options capability for additional push metadata
    PushOptions,
    /// Object-format capability for specifying hash algorithm
    ObjectFormat(String),
    /// Session-id capability for session tracking
    SessionId(String),
    /// Filter capability for partial clone support
    Filter(String),
    /// Symref capability for symbolic reference information
    Symref(String),
    /// Agent capability for client/server identification
    Agent(String),
    /// Unknown capability for forward compatibility
    Unknown(String),
}

impl FromStr for Capability {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Parameterized capabilities
        if let Some(rest) = s.strip_prefix("agent=") {
            return Ok(Capability::Agent(rest.to_string()));
        }
        if let Some(rest) = s.strip_prefix("session-id=") {
            return Ok(Capability::SessionId(rest.to_string()));
        }
        if let Some(rest) = s.strip_prefix("push-cert=") {
            return Ok(Capability::PushCert(rest.to_string()));
        }
        if let Some(rest) = s.strip_prefix("object-format=") {
            return Ok(Capability::ObjectFormat(rest.to_string()));
        }
        if let Some(rest) = s.strip_prefix("filter=") {
            return Ok(Capability::Filter(rest.to_string()));
        }
        if let Some(rest) = s.strip_prefix("symref=") {
            return Ok(Capability::Symref(rest.to_string()));
        }

        match s {
            "multi_ack" => Ok(Capability::MultiAck),
            "multi_ack_detailed" => Ok(Capability::MultiAckDetailed),
            "no-done" => Ok(Capability::NoDone),
            "side-band" => Ok(Capability::SideBand),
            "side-band-64k" => Ok(Capability::SideBand64k),
            "report-status" => Ok(Capability::ReportStatus),
            "report-status-v2" => Ok(Capability::ReportStatusv2),
            "ofs-delta" => Ok(Capability::OfsDelta),
            "deepen-since" => Ok(Capability::DeepenSince),
            "deepen-not" => Ok(Capability::DeepenNot),
            "deepen-relative" => Ok(Capability::DeepenRelative),
            "thin-pack" => Ok(Capability::ThinPack),
            "shallow" => Ok(Capability::Shallow),
            "include-tag" => Ok(Capability::IncludeTag),
            "delete-refs" => Ok(Capability::DeleteRefs),
            "quiet" => Ok(Capability::Quiet),
            "atomic" => Ok(Capability::Atomic),
            "no-thin" => Ok(Capability::NoThin),
            "no-progress" => Ok(Capability::NoProgress),
            "allow-tip-sha1-in-want" => Ok(Capability::AllowTipSha1InWant),
            "allow-reachable-sha1-in-want" => Ok(Capability::AllowReachableSha1InWant),
            "push-options" => Ok(Capability::PushOptions),
            _ => Ok(Capability::Unknown(s.to_string())),
        }
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Capability::MultiAck => write!(f, "multi_ack"),
            Capability::MultiAckDetailed => write!(f, "multi_ack_detailed"),
            Capability::NoDone => write!(f, "no-done"),
            Capability::SideBand => write!(f, "side-band"),
            Capability::SideBand64k => write!(f, "side-band-64k"),
            Capability::ReportStatus => write!(f, "report-status"),
            Capability::ReportStatusv2 => write!(f, "report-status-v2"),
            Capability::OfsDelta => write!(f, "ofs-delta"),
            Capability::DeepenSince => write!(f, "deepen-since"),
            Capability::DeepenNot => write!(f, "deepen-not"),
            Capability::DeepenRelative => write!(f, "deepen-relative"),
            Capability::ThinPack => write!(f, "thin-pack"),
            Capability::Shallow => write!(f, "shallow"),
            Capability::IncludeTag => write!(f, "include-tag"),
            Capability::DeleteRefs => write!(f, "delete-refs"),
            Capability::Quiet => write!(f, "quiet"),
            Capability::Atomic => write!(f, "atomic"),
            Capability::NoThin => write!(f, "no-thin"),
            Capability::NoProgress => write!(f, "no-progress"),
            Capability::AllowTipSha1InWant => write!(f, "allow-tip-sha1-in-want"),
            Capability::AllowReachableSha1InWant => write!(f, "allow-reachable-sha1-in-want"),
            Capability::PushCert(value) => write!(f, "push-cert={}", value),
            Capability::PushOptions => write!(f, "push-options"),
            Capability::ObjectFormat(format) => write!(f, "object-format={}", format),
            Capability::SessionId(id) => write!(f, "session-id={}", id),
            Capability::Filter(filter) => write!(f, "filter={}", filter),
            Capability::Symref(symref) => write!(f, "symref={}", symref),
            Capability::Agent(agent) => write!(f, "agent={}", agent),
            Capability::Unknown(s) => write!(f, "{}", s),
        }
    }
}

/// Side-band types for multiplexed data streams
pub enum SideBand {
    /// Sideband 1 contains packfile data
    PackfileData,
    /// Sideband 2 contains progress information
    ProgressInfo,
    /// Sideband 3 contains error information
    Error,
}

impl SideBand {
    pub fn value(&self) -> u8 {
        match self {
            Self::PackfileData => b'\x01',
            Self::ProgressInfo => b'\x02',
            Self::Error => b'\x03',
        }
    }
}

/// Reference types in Git
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum RefTypeEnum {
    Branch,
    Tag,
}

/// Git reference information
#[derive(Clone, Debug)]
pub struct GitRef {
    pub name: String,
    pub hash: String,
}

/// Reference command for push operations
#[derive(Debug, Clone)]
pub struct RefCommand {
    pub old_hash: String,
    pub new_hash: String,
    pub ref_name: String,
    pub ref_type: RefTypeEnum,
    pub default_branch: bool,
    pub status: CommandStatus,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone)]
pub enum CommandStatus {
    Pending,
    Success,
    Failed,
}

impl RefCommand {
    pub fn new(old_hash: String, new_hash: String, ref_name: String) -> Self {
        // Determine ref type based on ref name
        let ref_type = if ref_name.starts_with("refs/tags/") {
            RefTypeEnum::Tag
        } else {
            RefTypeEnum::Branch
        };

        Self {
            old_hash,
            new_hash,
            ref_name,
            ref_type,
            default_branch: false,
            status: CommandStatus::Pending,
            error_message: None,
        }
    }

    pub fn failed(&mut self, error: String) {
        self.status = CommandStatus::Failed;
        self.error_message = Some(error);
    }

    pub fn success(&mut self) {
        self.status = CommandStatus::Success;
        self.error_message = None;
    }

    pub fn get_status(&self) -> String {
        match &self.status {
            CommandStatus::Success => format!("ok {}", self.ref_name),
            CommandStatus::Failed => {
                let error = self.error_message.as_deref().unwrap_or("unknown error");
                format!("ng {} {}", self.ref_name, error)
            }
            CommandStatus::Pending => format!("ok {}", self.ref_name), // Default to ok for pending
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum CommandType {
    Create,
    Update,
    Delete,
}

/// Zero object ID constant
pub const ZERO_ID: &str = "0000000000000000000000000000000000000000";

/// Protocol constants
pub const LF: char = '\n';
pub const SP: char = ' ';
pub const NUL: char = '\0';
pub const PKT_LINE_END_MARKER: &[u8; 4] = b"0000";

// Git protocol capability lists
pub const RECEIVE_CAP_LIST: &str =
    "report-status report-status-v2 delete-refs quiet atomic no-thin ";
pub const COMMON_CAP_LIST: &str = "side-band-64k ofs-delta agent=git-internal/0.1.0";
pub const UPLOAD_CAP_LIST: &str = "multi_ack_detailed no-done include-tag ";
