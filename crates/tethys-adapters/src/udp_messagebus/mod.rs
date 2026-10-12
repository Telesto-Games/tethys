//! A client for Unreal's UDP message bus (the `UdpMessaging` plugin), the transport Session
//! Frontend uses to find editors and drive their automation workers.
//!
//! Layers, bottom up:
//! - [`wire`]: `FArchive` encoding and the segment formats.
//! - [`cbor`]: message bodies, in UE's CBOR dialect.
//! - [`message`]: a reassembled message's envelope (type, addresses, scope, body).
//! - [`node`]: sockets, discovery, segmenting, acknowledgements.
//! - [`automation`]: the session and automation-worker messages.
//! - [`runner`]: the `TestRunner` port on top.
//!
//! Checked against UE 5.8 (protocol 18). `examples/msgbus_probe.rs` walks the
//! whole conversation and can dump messages; `examples/test_runner_probe.rs`
//! drives `TestRunner` against a project.

pub mod automation;
pub mod cbor;
pub mod message;
pub mod node;
pub mod runner;
pub mod wire;
