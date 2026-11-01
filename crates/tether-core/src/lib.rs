//! Hardware-independent protocol core for Android USB tethering on macOS.
//!
//! The crate deliberately contains no USB, macOS or privileged code.  All
//! parsers take untrusted byte slices, validate lengths and offsets before
//! reading, and report structured errors.  The USB transport and the Ethernet
//! bridge are traits so that the complete state machine can be exercised
//! without a phone or elevated privileges.
//!
//! Normative basis:
//! - [MS-RNDIS] Remote Network Driver Interface Specification, 2014-05-01
//! - Microsoft, Remote NDIS to USB Mapping
//! - USB-IF CDC class definitions (ECM/NCM)

pub mod cdc;
pub mod descriptor;
pub mod engine;
pub mod error;
pub mod ethernet;
pub mod net;
pub mod rndis;
pub mod testkit;
pub mod transport;

pub use descriptor::{classify_configuration, parse_configuration, Protocol};
pub use error::TetherError;
pub use rndis::session::{RndisSession, SessionInfo, SessionStats};
