use std::fmt;

/// Stable diagnostic categories.  The CLI and the status file use these names,
/// so they must not change silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCategory {
    UsbNotFound,
    UsbPermission,
    UsbBusy,
    UsbTransfer,
    UsbTimeout,
    UnsupportedDevice,
    MalformedDescriptor,
    ProtocolViolation,
    ProtocolTimeout,
    LinkDown,
    Network,
    HelperUnavailable,
    PermissionRequired,
    Configuration,
    Cancelled,
    Internal,
}

impl ErrorCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UsbNotFound => "usb_not_found",
            Self::UsbPermission => "usb_permission",
            Self::UsbBusy => "usb_busy",
            Self::UsbTransfer => "usb_transfer",
            Self::UsbTimeout => "usb_timeout",
            Self::UnsupportedDevice => "unsupported_device",
            Self::MalformedDescriptor => "malformed_descriptor",
            Self::ProtocolViolation => "protocol_violation",
            Self::ProtocolTimeout => "protocol_timeout",
            Self::LinkDown => "link_down",
            Self::Network => "network",
            Self::HelperUnavailable => "helper_unavailable",
            Self::PermissionRequired => "permission_required",
            Self::Configuration => "configuration",
            Self::Cancelled => "cancelled",
            Self::Internal => "internal",
        }
    }

    /// A short, actionable hint.  Never includes payload bytes.
    pub fn hint(self) -> &'static str {
        match self {
            Self::UsbNotFound => "check the cable, enable USB tethering on the phone, then retry",
            Self::UsbPermission => "grant the process access to the USB device",
            Self::UsbBusy => "another process or a native macOS driver owns a required interface",
            Self::UsbTransfer => "check the cable and retry; the USB transfer failed",
            Self::UsbTimeout => "the device did not answer in time; replug it if this persists",
            Self::UnsupportedDevice => {
                "this USB configuration is not a supported RNDIS tethering function"
            }
            Self::MalformedDescriptor => {
                "the device sent an invalid USB descriptor; refusing to continue"
            }
            Self::ProtocolViolation => {
                "the device violated the RNDIS protocol; refusing to continue"
            }
            Self::ProtocolTimeout => "the RNDIS device stopped responding; reconnecting",
            Self::LinkDown => "the phone reported that the tethering link is down",
            Self::Network => "the host Ethernet bridge reported an error",
            Self::HelperUnavailable => {
                "start android-tether-helper with root privileges (see README)"
            }
            Self::PermissionRequired => "this operation needs the narrowly scoped root helper",
            Self::Configuration => "check the command line options",
            Self::Cancelled => "operation cancelled",
            Self::Internal => "unexpected internal error; see the log",
        }
    }
}

impl fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The single error type crossing module boundaries.  Messages are written for
/// humans and must never contain packet payloads or credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TetherError {
    pub category: ErrorCategory,
    pub message: String,
}

impl TetherError {
    pub fn new(category: ErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::UnsupportedDevice, message)
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::ProtocolViolation, message)
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::ProtocolTimeout, message)
    }

    pub fn descriptor(message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::MalformedDescriptor, message)
    }

    pub fn network(message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Network, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Internal, message)
    }

    pub fn category(&self) -> ErrorCategory {
        self.category
    }
}

impl fmt::Display for TetherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.category, self.message)
    }
}

impl std::error::Error for TetherError {}

pub type Result<T> = std::result::Result<T, TetherError>;
