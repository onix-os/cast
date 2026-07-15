//! Retained marker validation and authorized state-slot hardlinks.

use super::*;

impl RetainedTreeMarker {
    pub(crate) fn token(&self) -> &TreeToken {
        &self.token
    }

    pub(crate) fn needs_slot_link_authorization(&self) -> bool {
        self.authorized_links.load(Ordering::Acquire) == 2 && !self.slot_link_authorized.load(Ordering::Acquire)
    }

    /// Require a proposed recovery entry to be the sole extra hardlink to this
    /// exact retained marker inode. Placement and wrapper layout are proved by
    /// the transition namespace before it calls
    /// [`Self::authorize_recovered_slot_link`].
    pub(crate) fn require_recovery_slot_link_candidate(&self, file: &File, path: &Path) -> Result<(), TreeMarkerError> {
        let links = self.authorized_links.load(Ordering::Acquire);
        if links != 2 {
            return Err(TreeMarkerError::InvalidAuthorizedLinkCount {
                path: self.path.clone(),
                links,
            });
        }
        let actual = canonical_witness_with_links(file, path, 2)?;
        if !same_marker_inode(actual, self.witness) {
            return Err(TreeMarkerError::MarkerChanged { path: path.to_owned() });
        }
        let token = read_and_decode_with_links(file, actual, path, 2)?;
        require_expected_token(Some(&self.token), &token, path)
    }

    /// Mark the already-proved second link as authorized. This method does not
    /// discover or bless a pathname; callers must first prove exact placement,
    /// exact wrapper contents, and uniqueness from `nlink=2`.
    pub(crate) fn authorize_recovered_slot_link(&self) -> Result<(), TreeMarkerError> {
        let links = self.authorized_links.load(Ordering::Acquire);
        if links != 2 {
            return Err(TreeMarkerError::InvalidAuthorizedLinkCount {
                path: self.path.clone(),
                links,
            });
        }
        self.slot_link_authorized.store(true, Ordering::Release);
        Ok(())
    }

    /// Publish the only authorized state-slot hardlink from this retained
    /// descriptor. Existing names are never adopted by this in-process path.
    pub(crate) fn link_state_slot_noreplace(
        &self,
        store: &TreeMarkerStore,
        destination: &File,
        name: &CStr,
        path: &Path,
    ) -> Result<(), TreeMarkerError> {
        self.revalidate(store)?;
        let links = self.authorized_links.load(Ordering::Acquire);
        if links != 1 {
            return Err(TreeMarkerError::InvalidAuthorizedLinkCount {
                path: self.path.clone(),
                links,
            });
        }
        match link_retained_file_noreplace(&self.file, destination, name) {
            Ok(()) => {}
            Err(source) if source.raw_os_error() == Some(nix::libc::EEXIST) => {
                return Err(TreeMarkerError::SlotLinkPublicationCollision { path: path.to_owned() });
            }
            Err(source) => return Err(io_error("publish authorized tree-marker slot link", path, source)),
        }

        let linked = canonical_witness_with_links(&self.file, &self.path, 2)?;
        if !same_marker_inode(linked, self.witness) {
            return Err(TreeMarkerError::MarkerChanged {
                path: self.path.clone(),
            });
        }
        self.authorized_links.store(2, Ordering::Release);
        self.slot_link_authorized.store(true, Ordering::Release);
        self.file
            .sync_all()
            .map_err(|source| io_error("sync tree marker after slot-link publication", &self.path, source))?;
        self.revalidate(store)
    }

    pub(crate) fn require_authorized_slot_link(&self, file: &File, path: &Path) -> Result<(), TreeMarkerError> {
        if self.needs_slot_link_authorization() {
            return Err(TreeMarkerError::UnauthorizedSlotLink {
                path: self.path.clone(),
            });
        }
        self.require_recovery_slot_link_candidate(file, path)
    }

    /// Reopen the canonical name through another retained store while keeping
    /// this marker's already-authorized link-count contract.
    pub(crate) fn read_named_for_transition(
        &self,
        store: &TreeMarkerStore,
    ) -> Result<RetainedTreeMarker, TreeMarkerError> {
        self.require_slot_link_authorized()?;
        let links = self.authorized_links.load(Ordering::Acquire);
        let named = store
            .load_canonical_with_links(links, true)?
            .ok_or_else(|| TreeMarkerError::Missing {
                path: store.marker_path(),
            })?;
        require_expected_token(Some(&self.token), &named.token, &named.path)?;
        self.require_same_marker(&named)?;
        named.revalidate(store)?;
        Ok(named)
    }

    /// Bind a marker reopened through a current pathname to this retained
    /// marker inode, not merely to a copy of its token bytes.
    pub(crate) fn require_same_marker(&self, named: &Self) -> Result<(), TreeMarkerError> {
        self.require_slot_link_authorized()?;
        named.require_slot_link_authorized()?;
        if same_marker_inode(self.witness, named.witness)
            && self.authorized_links.load(Ordering::Acquire) == named.authorized_links.load(Ordering::Acquire)
            && self.token == named.token
        {
            Ok(())
        } else {
            Err(TreeMarkerError::MarkerChanged {
                path: named.path.clone(),
            })
        }
    }

    /// Prove that both the retained descriptor and canonical name still denote
    /// the exact decoded marker. This is intended for every trigger boundary.
    pub(crate) fn revalidate(&self, store: &TreeMarkerStore) -> Result<(), TreeMarkerError> {
        self.require_slot_link_authorized()?;
        store.validate_usr()?;
        store.reject_temporary()?;
        let links = self.authorized_links.load(Ordering::Acquire);
        let expected = marker_witness_with_links(self.witness, links);
        if canonical_witness_with_links(&self.file, &self.path, links)? != expected {
            return Err(TreeMarkerError::MarkerChanged {
                path: self.path.clone(),
            });
        }
        let retained_token = read_and_decode_with_links(&self.file, expected, &self.path, links)?;
        require_expected_token(Some(&self.token), &retained_token, &self.path)?;
        let named = store
            .load_canonical_with_links(links, true)?
            .ok_or_else(|| TreeMarkerError::MarkerChanged {
                path: self.path.clone(),
            })?;
        if !same_marker_inode(named.witness, self.witness) || named.token != self.token {
            return Err(TreeMarkerError::MarkerChanged {
                path: self.path.clone(),
            });
        }
        Ok(())
    }

    fn require_slot_link_authorized(&self) -> Result<(), TreeMarkerError> {
        if self.needs_slot_link_authorization() {
            Err(TreeMarkerError::UnauthorizedSlotLink {
                path: self.path.clone(),
            })
        } else {
            Ok(())
        }
    }
}
