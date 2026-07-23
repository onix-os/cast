use std::{
    ffi::OsString,
    os::unix::ffi::OsStrExt as _,
    path::{Path, PathBuf},
};

use super::{
    super::active_reblit_boot_projection::BootAssetRole, ActiveReblitBootRenderInputsError, BootRenderInputPolicy,
    PreparedActiveReblitBootSchemas, PreparedActiveReblitKernelRenderInput, PreparedActiveReblitStoneBootInputs,
    RetainedBootAssetCoordinate, RevalidatedActiveReblitBootStateRoots, allocation, error, require_deadline, state,
};

pub(super) fn derive_systemd_boot_coordinate(
    stone: &PreparedActiveReblitStoneBootInputs,
    deadline: std::time::Instant,
) -> Result<RetainedBootAssetCoordinate, ActiveReblitBootRenderInputsError> {
    let mut found = None;
    let mut count = 0usize;
    for (binding_index, asset) in stone.assets().enumerate() {
        require_deadline(deadline, "systemd-boot coordinate inventory", std::time::Instant::now())?;
        if !matches!(asset.role(), BootAssetRole::SystemdBoot) {
            continue;
        }
        count = count.saturating_add(1);
        if count > 1 {
            return Err(ActiveReblitBootRenderInputsError::SystemdBootCoordinateCount { actual: count });
        }
        found = Some(coordinate(binding_index, &asset)?);
    }
    let coordinate = found.ok_or(ActiveReblitBootRenderInputsError::SystemdBootCoordinateCount { actual: 0 })?;
    if bind_systemd_boot_coordinate(stone, &coordinate).is_none() {
        return Err(ActiveReblitBootRenderInputsError::SystemdBootCoordinateChanged {
            binding_index: coordinate.binding_index,
        });
    }
    require_deadline(deadline, "terminal systemd-boot coordinate", std::time::Instant::now())?;
    Ok(coordinate)
}

pub(super) fn derive_kernel_seeds(
    stone: &PreparedActiveReblitStoneBootInputs,
    roots: &RevalidatedActiveReblitBootStateRoots<'_>,
    schemas: &PreparedActiveReblitBootSchemas,
    policy: BootRenderInputPolicy,
    deadline: std::time::Instant,
) -> Result<Vec<PreparedActiveReblitKernelRenderInput>, ActiveReblitBootRenderInputsError> {
    let mut seeds = Vec::new();
    seeds
        .try_reserve_exact(stone.kernel_count().min(policy.max_kernels))
        .map_err(|source| allocation("canonical kernel coordinates", source))?;

    for (binding_index, asset) in stone.assets().enumerate() {
        require_deadline(deadline, "canonical Stone kernel inventory", std::time::Instant::now())?;
        let BootAssetRole::Kernel { version } = asset.role() else {
            continue;
        };
        if !roots.eligible_state_ids().contains(&asset.state_id())
            || schemas.schema_for_state(asset.state_id()).is_none()
        {
            continue;
        }
        if seeds.iter().any(|seed: &PreparedActiveReblitKernelRenderInput| {
            seed.state_id == asset.state_id() && seed.version.as_ref() == version
        }) {
            return Err(error::duplicate_kernel(asset.state_id(), version));
        }
        let actual = seeds.len().saturating_add(1);
        if actual > policy.max_kernels {
            return Err(ActiveReblitBootRenderInputsError::KernelCountLimit {
                limit: policy.max_kernels,
                actual,
            });
        }
        let version = clone_version(version)?;
        let kernel = coordinate(binding_index, &asset)?;
        if bind_kernel_coordinate(stone, &kernel, asset.state_id(), &version).is_none() {
            return Err(ActiveReblitBootRenderInputsError::KernelCoordinateChanged {
                binding_index: kernel.binding_index,
            });
        }
        let initrds = collect_initrds(stone, asset.state_id(), &version, deadline)?;
        seeds.push(PreparedActiveReblitKernelRenderInput {
            state_id: asset.state_id(),
            version,
            kernel,
            initrds,
        });
    }
    if seeds.is_empty() {
        return Err(ActiveReblitBootRenderInputsError::NoRenderableKernel);
    }
    require_deadline(deadline, "terminal canonical kernel join", std::time::Instant::now())?;
    Ok(seeds)
}

fn collect_initrds(
    stone: &PreparedActiveReblitStoneBootInputs,
    state_id: state::Id,
    version: &str,
    deadline: std::time::Instant,
) -> Result<Box<[RetainedBootAssetCoordinate]>, ActiveReblitBootRenderInputsError> {
    require_deadline(deadline, "initrd count admission", std::time::Instant::now())?;
    let count = stone
        .assets()
        .filter(|asset| {
            asset.state_id() == state_id
                && matches!(asset.role(), BootAssetRole::Initrd { version: candidate } if candidate == version)
        })
        .count();
    require_deadline(deadline, "initrd allocation admission", std::time::Instant::now())?;
    let mut initrds = Vec::new();
    initrds
        .try_reserve_exact(count)
        .map_err(|source| allocation("initrd binding coordinates", source))?;
    for (binding_index, asset) in stone.assets().enumerate() {
        require_deadline(deadline, "matching initrd inventory", std::time::Instant::now())?;
        if asset.state_id() != state_id
            || !matches!(asset.role(), BootAssetRole::Initrd { version: candidate } if candidate == version)
        {
            continue;
        }
        let coordinate = coordinate(binding_index, &asset)?;
        if bind_initrd_coordinate(stone, &coordinate, state_id, version).is_none() {
            return Err(ActiveReblitBootRenderInputsError::InitrdCoordinateChanged {
                binding_index: coordinate.binding_index,
            });
        }
        initrds.push(coordinate);
    }
    Ok(initrds.into_boxed_slice())
}

pub(super) fn revalidate_asset_coordinates(
    stone: &PreparedActiveReblitStoneBootInputs,
    systemd_boot: &RetainedBootAssetCoordinate,
    kernels: &[PreparedActiveReblitKernelRenderInput],
    deadline: std::time::Instant,
) -> Result<(), ActiveReblitBootRenderInputsError> {
    require_deadline(
        deadline,
        "systemd-boot coordinate revalidation",
        std::time::Instant::now(),
    )?;
    if bind_systemd_boot_coordinate(stone, systemd_boot).is_none() {
        return Err(ActiveReblitBootRenderInputsError::SystemdBootCoordinateChanged {
            binding_index: systemd_boot.binding_index,
        });
    }
    for kernel in kernels {
        require_deadline(deadline, "kernel coordinate revalidation", std::time::Instant::now())?;
        if bind_kernel_coordinate(stone, &kernel.kernel, kernel.state_id, &kernel.version).is_none() {
            return Err(ActiveReblitBootRenderInputsError::KernelCoordinateChanged {
                binding_index: kernel.kernel.binding_index,
            });
        }
        for initrd in &kernel.initrds {
            require_deadline(deadline, "initrd coordinate revalidation", std::time::Instant::now())?;
            if bind_initrd_coordinate(stone, initrd, kernel.state_id, &kernel.version).is_none() {
                return Err(ActiveReblitBootRenderInputsError::InitrdCoordinateChanged {
                    binding_index: initrd.binding_index,
                });
            }
        }
    }
    Ok(())
}

pub(super) fn bind_systemd_boot_coordinate<'a>(
    stone: &'a PreparedActiveReblitStoneBootInputs,
    coordinate: &RetainedBootAssetCoordinate,
) -> Option<super::BoundActiveReblitBootAsset<'a>> {
    bind_coordinate(stone, coordinate, coordinate.state_id, |role| {
        matches!(role, BootAssetRole::SystemdBoot)
    })
}

pub(super) fn bind_kernel_coordinate<'a>(
    stone: &'a PreparedActiveReblitStoneBootInputs,
    coordinate: &RetainedBootAssetCoordinate,
    state_id: state::Id,
    version: &str,
) -> Option<super::BoundActiveReblitBootAsset<'a>> {
    bind_coordinate(
        stone,
        coordinate,
        state_id,
        |role| matches!(role, BootAssetRole::Kernel { version: candidate } if candidate == version),
    )
}

pub(super) fn bind_initrd_coordinate<'a>(
    stone: &'a PreparedActiveReblitStoneBootInputs,
    coordinate: &RetainedBootAssetCoordinate,
    state_id: state::Id,
    version: &str,
) -> Option<super::BoundActiveReblitBootAsset<'a>> {
    bind_coordinate(
        stone,
        coordinate,
        state_id,
        |role| matches!(role, BootAssetRole::Initrd { version: candidate } if candidate == version),
    )
}

fn bind_coordinate<'a>(
    stone: &'a PreparedActiveReblitStoneBootInputs,
    coordinate: &RetainedBootAssetCoordinate,
    state_id: state::Id,
    role_matches: impl FnOnce(&BootAssetRole) -> bool,
) -> Option<super::BoundActiveReblitBootAsset<'a>> {
    let asset = stone.asset_at(usize::from(coordinate.binding_index))?;
    (coordinate.state_id == state_id
        && asset.state_id() == state_id
        && asset.digest() == coordinate.digest
        && asset.length() == coordinate.length
        && asset.logical_path() == coordinate.logical_path
        && role_matches(asset.role()))
    .then_some(asset)
}

fn coordinate(
    binding_index: usize,
    asset: &super::BoundActiveReblitBootAsset<'_>,
) -> Result<RetainedBootAssetCoordinate, ActiveReblitBootRenderInputsError> {
    Ok(RetainedBootAssetCoordinate {
        binding_index: u16::try_from(binding_index).map_err(|_| {
            ActiveReblitBootRenderInputsError::BindingIndexLimit {
                limit: u16::MAX as usize,
                actual: binding_index,
            }
        })?,
        state_id: asset.state_id(),
        digest: asset.digest(),
        length: asset.length(),
        logical_path: clone_logical_path(asset.logical_path())?,
    })
}

fn clone_logical_path(path: &Path) -> Result<PathBuf, ActiveReblitBootRenderInputsError> {
    let mut cloned = OsString::new();
    cloned
        .try_reserve_exact(path.as_os_str().as_bytes().len())
        .map_err(|source| allocation("boot asset logical path", source))?;
    cloned.push(path);
    Ok(PathBuf::from(cloned))
}

fn clone_version(version: &str) -> Result<Box<str>, ActiveReblitBootRenderInputsError> {
    let mut cloned = String::new();
    cloned
        .try_reserve_exact(version.len())
        .map_err(|source| allocation("kernel version", source))?;
    cloned.push_str(version);
    Ok(cloned.into_boxed_str())
}
