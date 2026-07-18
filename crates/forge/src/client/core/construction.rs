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
    pub fn build(self) -> Result<Client, Error> {
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
        let mut system = open_mutable_system_capabilities(self.installation)?;

        let startup_gate = startup_gate::CleanSystemStartup::enter(&system, &active_state_reservation).map_err(
            |source| Error::SystemStartupGate {
                source: Box::new(source),
            },
        );
        let namespace = system.installation().revalidate_mutable_namespace();
        namespace?;
        let startup_gate = startup_gate?;
        let active_state = active_state_reservation.discover_after_startup_gate(system.installation(), &startup_gate);
        let namespace = system.installation().revalidate_mutable_namespace();
        namespace?;
        let active_state = active_state?;

        let active_state_proof = active_state.revalidate(system.installation());
        let namespace = system.installation().revalidate_mutable_namespace();
        namespace?;
        active_state_proof?;
        let system_model = if let Some(path) = self.system_intent_path {
            system_model::load(&path)
                .map_err(Error::from)
                .and_then(|model| model.ok_or(Error::ImportSystemIntentDoesntExist(path)))
                .map(Some)
        } else {
            startup_gate
                .load_default_system_intent(system.installation(), &active_state)
                .map_err(|source| Error::SystemStartupGate {
                    source: Box::new(source),
                })
        };
        let namespace = system.installation().revalidate_mutable_namespace();
        namespace?;
        let system_model = system_model?;
        system.set_system_model(system_model);
        let active_state_proof = active_state.revalidate(system.installation());
        let namespace = system.installation().revalidate_mutable_namespace();
        namespace?;
        active_state_proof?;

        let config = config::Manager::system(&system.installation().root, "cast");
        system.installation().revalidate_mutable_namespace()?;
        let repositories = if let Some(repos) = self.repositories {
            repository::Manager::with_explicit(&self.client_name, repos, system.installation().clone())
        } else if let Some(system_model) = &system.installation().system_model {
            repository::Manager::with_system_model(
                &self.client_name,
                system_model.clone(),
                system.installation().clone(),
            )
        } else {
            repository::Manager::with_config_manager(config.clone(), system.installation().clone())
        };
        let namespace = system.installation().revalidate_mutable_namespace();
        namespace?;
        let repositories = repositories?;

        let registry = build_registry(active_state.active(), &repositories, system.install_db(), system.state_db());
        let namespace = system.installation().revalidate_mutable_namespace();
        namespace?;
        let registry = registry?;
        let active_state_proof = active_state.revalidate(system.installation());
        let namespace = system.installation().revalidate_mutable_namespace();
        namespace?;
        active_state_proof?;
        drop(startup_gate);
        drop(active_state);
        let mut client = system.into_client(registry, config, repositories);

        if let Some(blit_root) = self.blit_root {
            client = client.ephemeral(blit_root)?;
        }
        if let Some(verbose) = self.system_intent_notice {
            print_system_intent_notice(&client, verbose);
        }
        Ok(client)
    }
}

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
