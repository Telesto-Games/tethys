//! Tethys domain, use cases and ports.
//!
//! This crate must not depend on UI, OS or agent details. Adapters live in
//! `tethys-adapters`; the composition root is `tethys-app`.

pub mod domain;
pub mod ports;
pub mod unreal;
pub mod usecases;

pub use domain::{
    AdapterKind, AgentProfile, AssociationKind, DomainError, Project, ProjectError, SessionId,
};
