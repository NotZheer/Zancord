//! Peer identity types.
//!
//! These are type aliases (not newtypes) so protocol types stay transparent and
//! easy to serialize; they document intent at call sites.

pub type PeerId = String;
pub type Username = String;
