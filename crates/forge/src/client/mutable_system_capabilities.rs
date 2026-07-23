//! Coherent mutable-system capabilities retained through client startup.
//!
//! The installation and its three anchored database handles are opened and
//! transported as one opaque value. Startup recovery cannot represent a
//! mixed-root tuple because it accepts this aggregate rather than independent
//! components.

use crate::{Installation, Registry, db, installation, repository};

use super::{Client, Error, Scope};

pub(super) struct MutableSystemCapabilities {
    install_db: db::meta::Database,
    state_db: db::state::Database,
    layout_db: db::layout::Database,
    // Deliberately last: Rust drops fields in declaration order, so the
    // retained namespace/global lock outlives every database handle.
    installation: Installation,
}

/// The sole production constructor for mutable-system capabilities.
pub(super) fn open_mutable_system_capabilities(installation: Installation) -> Result<MutableSystemCapabilities, Error> {
    installation.revalidate_mutable_namespace()?;
    let install = installation.mutable_database_location(installation::DatabaseKind::Install)?;
    let state = installation.mutable_database_location(installation::DatabaseKind::State)?;
    let layout = installation.mutable_database_location(installation::DatabaseKind::Layout)?;

    let (install_url, install_anchor) = install.parts();
    let install_db = db::meta::Database::new_mutable_system_anchored(install_url, install_anchor);
    after_system_database_open(installation::DatabaseKind::Install);
    let alias = install.revalidate();
    let namespace = installation.revalidate_mutable_namespace();
    namespace?;
    alias?;
    let install_db = install_db?;

    let (state_url, state_anchor) = state.parts();
    let state_db = db::state::Database::new_anchored(state_url, state_anchor);
    after_system_database_open(installation::DatabaseKind::State);
    let alias = state.revalidate();
    let namespace = installation.revalidate_mutable_namespace();
    namespace?;
    alias?;
    let state_db = state_db?;

    let (layout_url, layout_anchor) = layout.parts();
    let layout_db = db::layout::Database::new_anchored(layout_url, layout_anchor);
    after_system_database_open(installation::DatabaseKind::Layout);
    let alias = layout.revalidate();
    let namespace = installation.revalidate_mutable_namespace();
    namespace?;
    alias?;
    let layout_db = layout_db?;

    installation.revalidate_mutable_namespace()?;
    Ok(MutableSystemCapabilities {
        install_db,
        state_db,
        layout_db,
        installation,
    })
}

impl MutableSystemCapabilities {
    pub(super) fn installation(&self) -> &Installation {
        &self.installation
    }

    pub(super) fn set_system_model(&mut self, system_model: Option<crate::system_model::LoadedSystemModel>) {
        self.installation.system_model = system_model;
    }

    pub(super) fn install_db(&self) -> &db::meta::Database {
        &self.install_db
    }

    pub(super) fn state_db(&self) -> &db::state::Database {
        &self.state_db
    }

    #[cfg(test)]
    pub(in crate::client) fn layout_db(&self) -> &db::layout::Database {
        &self.layout_db
    }

    /// Consume the aggregate directly into the correctly drop-ordered Client.
    /// No loose tuple of installation/database capabilities is representable.
    pub(super) fn into_client(
        self,
        registry: Registry,
        config: config::Manager,
        repositories: repository::Manager,
    ) -> Client {
        Client {
            registry,
            install_db: self.install_db,
            state_db: self.state_db,
            layout_db: self.layout_db,
            config: Some(config),
            repositories,
            scope: Scope::Stateful,
            installation: self.installation,
        }
    }

    #[cfg(test)]
    pub(in crate::client) fn from_test_parts(
        _seal: &MutableSystemCapabilitiesTestSeal,
        installation: Installation,
        state_db: db::state::Database,
        layout_db: db::layout::Database,
    ) -> Self {
        let install_db = db::meta::Database::new(":memory:").expect("open test mutable-system install database");
        Self {
            install_db,
            state_db,
            layout_db,
            installation,
        }
    }
}

/// Test-only permission for constructing semantic in-memory capability sets.
#[cfg(test)]
pub(in crate::client) struct MutableSystemCapabilitiesTestSeal {
    _private: (),
}

#[cfg(test)]
impl MutableSystemCapabilitiesTestSeal {
    pub(in crate::client) fn new() -> Self {
        Self { _private: () }
    }
}

#[cfg(test)]
std::thread_local! {
    static AFTER_SYSTEM_DATABASE_OPEN: std::cell::RefCell<Option<(installation::DatabaseKind, Box<dyn FnOnce()>)>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_after_system_database_open(
    kind: installation::DatabaseKind,
    hook: impl FnOnce() + 'static,
) {
    AFTER_SYSTEM_DATABASE_OPEN.with(|slot| {
        assert!(slot.borrow_mut().replace((kind, Box::new(hook))).is_none());
    });
}

#[cfg(test)]
fn after_system_database_open(kind: installation::DatabaseKind) {
    AFTER_SYSTEM_DATABASE_OPEN.with(|slot| {
        let armed = slot.borrow().as_ref().is_some_and(|(expected, _)| *expected == kind);
        if armed {
            let (_, hook) = slot.borrow_mut().take().expect("checked database-open hook");
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_system_database_open(_kind: installation::DatabaseKind) {}

#[cfg(test)]
mod tests;
