//! Platform services shared by file I/O, search and the application.
//!
//! Windows gets native identity and path handling where available. Other
//! platforms use the closest `std` facilities so the domain crates stay
//! portable and testable.

pub mod file_assoc;
pub mod identity;
pub mod paths;
pub mod process;
pub mod single_instance;

pub use file_assoc::{
    AssociationReport, FileAssocError, PROG_ID, SUPPORTED_EXTENSIONS, association_command_line,
    register_file_associations, unregister_file_associations,
};
pub use identity::{FileSnapshot, identity_of_path, read_file_stamp, snapshot_of_path};
pub use paths::{ProfilePaths, resolve_profile};
pub use process::{ChildProcess, ProcessError};
pub use single_instance::{
    Claim, ForwardedOpen, InstanceGuard, SingleInstanceError, claim, forward,
};
