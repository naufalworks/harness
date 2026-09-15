//! P15-T04: exporting and importing selected memories and continuation packets.
//!
//! Three rules shape this module.
//!
//! 1. **One sanitizer, one citation shape.** [`review`] owns both, and search
//!    (`storage::history`) uses the very same functions. An export can therefore never be more
//!    permissive than the search index, and a citation means the same thing on both sides.
//! 2. **Nothing leaves unreviewed.** A bundle is assembled as `draft`, an operator reviews the
//!    exact contents (pinned by checksum), and only then can it be `released` into a packet.
//!    Release recomputes the checksum, so contents that changed after review fail instead of
//!    leaving quietly.
//! 3. **Stable id plus revision, never a fresh id.** Items are addressed by the identifier they
//!    already have in the source database together with the revision they held when selected.
//!    That is what lets an import distinguish "already have exactly this" from "this is newer"
//!    from "this is older than what I hold", and it is why a round trip is idempotent rather
//!    than duplicating rows.

pub mod packet;
pub mod review;
