/// A builder for [`Client`]
pub struct ClientBuilder {
    client_name: String,
    installation: Installation,
    repositories: Option<repository::Map>,
    system_intent_path: Option<PathBuf>,
    system_intent_notice: Option<bool>,
    blit_root: Option<PathBuf>,
}

impl ClientBuilder {
    /// Set the repositories
    pub fn repositories(mut self, repositories: repository::Map) -> ClientBuilder {
        self.repositories = Some(repositories);
        self
    }

    /// Import user-authored Gluon system intent from the provided path.
    pub fn system_intent_path(mut self, path: impl Into<PathBuf>) -> ClientBuilder {
        self.system_intent_path = Some(path.into());
        self
    }

    /// Emit the interactive declarative-intent notice only after a complete,
    /// successful client build. Library callers remain silent by default.
    pub(crate) fn system_intent_notice(mut self, verbose: bool) -> ClientBuilder {
        self.system_intent_notice = Some(verbose);
        self
    }

    /// Set the client to an ephemeral client that doesn't record state changes
    /// and blits to a different root.
    ///
    /// This is useful for installing a root to a container (for example, Mason) while
    /// using a shared cache.
    ///
    /// Returns an error on construction if `blit_root` is the same as the installation
    /// root, since the system client should always be stateful.
    pub fn ephemeral(mut self, blit_root: impl Into<PathBuf>) -> ClientBuilder {
        self.blit_root = Some(blit_root.into());
        self
    }

    /// Build the [`Client`]
    pub fn build(mut self) -> Result<Client, Error> {
        // A system or ephemeral Client owns mutable databases, the startup
        // coordinator, and transition journals. Reject every non-mutable
        // installation mode before acquiring any of that authority. In
        // particular, an explicit read-only snapshot must never become a
        // mutable client merely because its underlying root is writable.
        if !self.installation.is_mutable_system() {
            return Err(Error::SystemInstallationRequired);
        }

        // Preserve the lock order used by every transition: cooperating-writer
        // coordinator first, retained journal lock second. Strict live-state
        // discovery is deliberately deferred until the databases, journal,
        // and orphan evidence have been inspected, but taking the coordinator
        // only after the journal would introduce an ABBA deadlock with an
        // in-flight transition.
        let active_state_reservation = active_state_snapshot::ActiveStateReservation::acquire()?;
        self.installation.revalidate_mutable_namespace()?;
        let (install_db, state_db, layout_db) = open_mutable_system_databases(&self.installation)?;

        let startup_gate = startup_gate::CleanSystemStartup::enter(&self.installation, &state_db).map_err(|source| {
            Error::SystemStartupGate {
                source: Box::new(source),
            }
        });
        let namespace = self.installation.revalidate_mutable_namespace();
        namespace?;
        let startup_gate = startup_gate?;
        let active_state = active_state_reservation.discover_after_startup_gate(&self.installation, &startup_gate);
        let namespace = self.installation.revalidate_mutable_namespace();
        namespace?;
        let active_state = active_state?;

        let active_state_proof = active_state.revalidate(&self.installation);
        let namespace = self.installation.revalidate_mutable_namespace();
        namespace?;
        active_state_proof?;
        let system_model = if let Some(path) = self.system_intent_path {
            system_model::load(&path)
                .map_err(Error::from)
                .and_then(|model| model.ok_or(Error::ImportSystemIntentDoesntExist(path)))
                .map(Some)
        } else {
            startup_gate
                .load_default_system_intent(&self.installation, &active_state)
                .map_err(|source| Error::SystemStartupGate {
                    source: Box::new(source),
                })
        };
        let namespace = self.installation.revalidate_mutable_namespace();
        namespace?;
        let system_model = system_model?;
        self.installation.system_model = system_model;
        let active_state_proof = active_state.revalidate(&self.installation);
        let namespace = self.installation.revalidate_mutable_namespace();
        namespace?;
        active_state_proof?;

        let config = config::Manager::system(&self.installation.root, "cast");
        self.installation.revalidate_mutable_namespace()?;
        let repositories = if let Some(repos) = self.repositories {
            repository::Manager::with_explicit(&self.client_name, repos, self.installation.clone())
        } else if let Some(system_model) = &self.installation.system_model {
            repository::Manager::with_system_model(&self.client_name, system_model.clone(), self.installation.clone())
        } else {
            repository::Manager::with_config_manager(config.clone(), self.installation.clone())
        };
        let namespace = self.installation.revalidate_mutable_namespace();
        namespace?;
        let repositories = repositories?;

        let registry = build_registry(active_state.active(), &repositories, &install_db, &state_db);
        let namespace = self.installation.revalidate_mutable_namespace();
        namespace?;
        let registry = registry?;
        let active_state_proof = active_state.revalidate(&self.installation);
        let namespace = self.installation.revalidate_mutable_namespace();
        namespace?;
        active_state_proof?;
        drop(startup_gate);
        drop(active_state);

        let mut client = Client {
            config: Some(config),
            installation: self.installation,
            repositories,
            registry,
            install_db,
            state_db,
            layout_db,
            scope: Scope::Stateful,
        };

        if let Some(blit_root) = self.blit_root {
            client = client.ephemeral(blit_root)?;
        }
        if let Some(verbose) = self.system_intent_notice {
            print_system_intent_notice(&client, verbose);
        }
        Ok(client)
    }
}

fn open_mutable_system_databases(
    installation: &Installation,
) -> Result<(db::meta::Database, db::state::Database, db::layout::Database), Error> {
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
    Ok((install_db, state_db, layout_db))
}

#[cfg(test)]
std::thread_local! {
    static AFTER_SYSTEM_DATABASE_OPEN: std::cell::RefCell<Option<(installation::DatabaseKind, Box<dyn FnOnce()>)>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn arm_after_system_database_open(kind: installation::DatabaseKind, hook: impl FnOnce() + 'static) {
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

fn print_system_intent_notice(client: &Client, verbose: bool) {
    if let Some(notice) = render_system_intent_notice(client, verbose) {
        emit_system_intent_notice(notice);
    }
}

fn render_system_intent_notice(client: &Client, verbose: bool) -> Option<String> {
    let Some(system_model) = client.system_intent() else {
        return None;
    };
    if system_model.disable_warning && !verbose {
        return None;
    }
    let path = system_model.path();
    let first_line = format!(
        "{}: authored Gluon system intent at {path:?} is active.",
        "INFO".green()
    );
    if system_model.disable_warning {
        return Some(first_line);
    }

    Some(format!(
        "{first_line}
Hence:
- This system intent is the source of truth and defines all
  repositories & installed packages.
- Any changes made via `cast` commands will be temporary
  until the authored intent is updated.
- The system state can be reverted to match the declared intent
  by doing a `cast sync`.
- Each state stores a generated `/usr/lib/system-model.glu` snapshot;
  it is not the authored source and should not be edited.
- To disable declarative system intent, remove or rename {path:?}.",
    ))
}

#[cfg(not(test))]
fn emit_system_intent_notice(notice: String) {
    eprintln!("{notice}");
}

#[cfg(test)]
std::thread_local! {
    static SYSTEM_INTENT_NOTICE_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce(String)>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn arm_system_intent_notice_capture(capture: impl FnOnce(String) + 'static) {
    SYSTEM_INTENT_NOTICE_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(capture)).is_none());
    });
}

#[cfg(test)]
fn disarm_system_intent_notice_capture() -> bool {
    SYSTEM_INTENT_NOTICE_CAPTURE.with(|slot| slot.borrow_mut().take().is_some())
}

#[cfg(test)]
fn emit_system_intent_notice(notice: String) {
    let capture = SYSTEM_INTENT_NOTICE_CAPTURE.with(|slot| slot.borrow_mut().take());
    if let Some(capture) = capture {
        capture(notice);
    } else {
        eprintln!("{notice}");
    }
}
