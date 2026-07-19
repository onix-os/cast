use super::*;
use serde_json::json;

fn os_info(former: Vec<serde_json::Value>) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "os-info-version": "0.1",
        "start_date": "2020-01-01T00:00:00Z",
        "metadata": {
            "identity": {
                "id": "current-os", "id_like": "linux", "name": "Current OS",
                "display": "Current OS Stable", "ansi_color": null,
                "former_identities": former
            },
            "maintainers": {},
            "version": {
                "full": "1.0.0", "short": "1", "build_id": "fixture",
                "released": "2026-01-01T00:00:00Z", "announcement": null, "codename": null
            }
        },
        "system": {
            "composition": { "bases": [], "technology": { "core": [], "optional": [] } },
            "features": {
                "atomic_updates": { "strategy": "atomic", "rollback_support": true },
                "boot": { "bootloader": "systemd-boot", "firmware": { "uefi": true, "secure_boot": false, "bios": false } },
                "filesystem": { "default": "ext4", "supported": ["ext4"] }
            },
            "kernel": { "type": "linux", "name": "linux" },
            "platform": { "architecture": "x86_64", "variant": "generic" },
            "update": {
                "strategy": "atomic",
                "cadence": {
                    "type": "rolling", "sync_interval": null, "sync_day": null,
                    "release_schedule": null, "support_timeline": null
                },
                "approach": "rolling"
            }
        },
        "resources": { "websites": {}, "social": {}, "funding": {} },
        "security_contact": null
    }))
    .unwrap()
}

fn signature(plan: &PreparedActiveReblitBootPublicationPlan) -> Vec<(PathBuf, u128, u64, Option<Vec<u8>>)> {
    plan.outputs()
        .iter()
        .map(|output| {
            (
                output.relative_path().to_owned(),
                output.source().digest(),
                output.source().length(),
                output.source().generated_bytes().map(<[u8]>::to_vec),
            )
        })
        .collect()
}

#[test]
fn historical_local_schema_is_used_and_unavailable_history_uses_sticky_global_fallback() {
    let deadline = support::future_deadline();
    let fixture = support::RenderFixture::new(
        support::StateSpec::one_kernel("6.12"),
        vec![
            support::StateSpec::one_kernel("6.10"),
            support::StateSpec::one_kernel("6.8"),
        ],
    );
    std::fs::remove_file(
        fixture
            .installation
            .root_path(fixture.histories[1].id.to_string())
            .join("usr/lib/os-release"),
    )
    .unwrap();
    with_render_inputs!(fixture, deadline, |fixture, inputs| {
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
        let topology = topology::alias_topology();
        let (plan, _) = fixture_plan(rendered, &topology);
        let local_state = i32::from(fixture.histories[0].id);
        let fallback_state = i32::from(fixture.histories[1].id);
        assert!(plan.outputs().iter().any(|output| {
            output.relative_path() == Path::new(&format!("loader/entries/history0-6.10-{local_state}.conf"))
        }));
        let fallback_path = format!("loader/entries/head-6.8-{fallback_state}.conf");
        let fallback = std::str::from_utf8(generated_at(&plan, &fallback_path)).unwrap();
        let kernel = checksum_payload_path("head", "vmlinuz", b"render kernel 6.8");
        assert!(fallback.starts_with(&format!("title Render Head (6.8)\nlinux /{kernel}\n")));
    });
}

#[test]
fn former_identities_emit_no_outputs_and_do_not_change_rendered_bytes() {
    let deadline = support::future_deadline();
    let former = vec![
        json!({
            "id": "former-one", "name": "Former One",
            "start_date": "2020-01-01T00:00:00Z", "end_date": "2021-01-01T00:00:00Z",
            "end_version": "1", "announcement": null
        }),
        json!({
            "id": "former-two", "name": "Former Two",
            "start_date": "2020-01-01T00:00:00Z", "end_date": "2021-01-01T00:00:00Z",
            "end_version": "1", "announcement": null
        }),
    ];
    let spec = support::StateSpec::one_kernel("6.12").with_cmdline("lib/os-info.json", os_info(former));
    let with_former = with_render_inputs!(
        support::RenderFixture::new(spec, Vec::new()),
        deadline,
        |fixture, inputs| {
            assert_eq!(inputs.global_schema().former_identities().len(), 2);
            let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
            let topology = topology::alias_topology();
            let (plan, _) = fixture_plan(rendered, &topology);
            assert_eq!(plan.outputs().len(), 5);
            assert!(plan.outputs().iter().all(|output| {
                let path = output.relative_path().to_string_lossy();
                !path.contains("former-one") && !path.contains("former-two")
            }));
            let state = i32::from(fixture.head.id);
            let entry = std::str::from_utf8(generated_at(
                &plan,
                &format!("loader/entries/current-os-6.12-{state}.conf"),
            ))
            .unwrap();
            assert!(entry.starts_with("title Current OS Stable (6.12)\n"));
            signature(&plan)
        }
    );

    let without_former = with_render_inputs!(
        support::RenderFixture::new(
            support::StateSpec::one_kernel("6.12").with_cmdline("lib/os-info.json", os_info(Vec::new())),
            Vec::new(),
        ),
        deadline,
        |_fixture, inputs| {
            assert!(inputs.global_schema().former_identities().is_empty());
            let topology = topology::alias_topology();
            let (plan, _) = fixture_plan(RenderedActiveReblitBlsRequests::render(&inputs).unwrap(), &topology);
            signature(&plan)
        }
    );
    assert_eq!(with_former, without_former);
}
