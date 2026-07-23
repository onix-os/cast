mod durability_faults;
mod evidence_races;
#[path = "../test_support.rs"]
mod fixture;
mod matrix;

use crate::client::{
    startup_gate,
    startup_recovery::{
        UsrExchangeParentDurabilityEvent, reset_usr_exchange_parent_durability_events,
        take_usr_exchange_parent_durability_events,
    },
};

use fixture::Fixture;

fn reset_events() {
    reset_usr_exchange_parent_durability_events();
    assert!(take_usr_exchange_parent_durability_events().is_empty());
}

fn take_events() -> Vec<UsrExchangeParentDurabilityEvent> {
    take_usr_exchange_parent_durability_events()
}

fn assert_success_events(fixture: &Fixture) {
    let ((staging_device, staging_inode), (root_device, root_inode)) = fixture.durability_parent_identities();
    assert_eq!(
        take_events(),
        vec![
            UsrExchangeParentDurabilityEvent::StagingParentSynced {
                device: staging_device,
                inode: staging_inode,
            },
            UsrExchangeParentDurabilityEvent::InstallationRootSynced {
                device: root_device,
                inode: root_inode,
            },
            UsrExchangeParentDurabilityEvent::FinalEvidenceRevalidated,
        ]
    );
}

fn assert_parent_durability_failure(error: startup_gate::Error) {
    assert!(matches!(error, startup_gate::Error::UsrExchangeParentDurability(_)));
}
