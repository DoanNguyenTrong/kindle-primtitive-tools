//! Primitive Store engine. Pure logic, with no network code: the catalog and manifest types, the resolver that picks a
//! version for a device, the verified and safe unpacking of a package, the install journal with rollback, and the record
//! of what is installed. The device's download and its file paths are given by the caller, so all of this is tested on
//! the Mac with fixtures.

pub mod catalog;
pub mod refresh;
pub mod install;
pub mod pipeline;
pub mod record;
pub mod resolve;
pub mod unpack;
pub mod verify;
pub mod version;

pub use version::Version;
