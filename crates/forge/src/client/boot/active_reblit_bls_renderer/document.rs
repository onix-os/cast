use std::path::Path;

#[cfg(test)]
use std::{cell::RefCell, path::PathBuf};

use super::{ActiveReblitBlsRendererError, RenderBudget, RenderedInitrdCandidate, allocation};

const LOADER_CONTROL_PATH: &str = "loader/loader.conf";

#[cfg(test)]
thread_local! {
    static MATERIALIZATION_STARTS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn render_loader_control(
    namespace: &str,
    budget: &mut RenderBudget,
) -> Result<Box<[u8]>, ActiveReblitBlsRendererError> {
    let length = "default \""
        .len()
        .checked_add(namespace.len())
        .and_then(|total| total.checked_add("*\"\n".len()))
        .unwrap_or(usize::MAX);
    budget.admit_generated(Path::new(LOADER_CONTROL_PATH), length)?;
    #[cfg(test)]
    record_materialization_start(Path::new(LOADER_CONTROL_PATH));
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|source| allocation("BLS loader control bytes", source))?;
    bytes.extend_from_slice(b"default \"");
    bytes.extend_from_slice(namespace.as_bytes());
    bytes.extend_from_slice(b"*\"\n");
    debug_assert_eq!(bytes.len(), length);
    Ok(bytes.into_boxed_slice())
}

pub(super) fn render_entry(
    entry_path: &Path,
    display_name: &str,
    version: &str,
    kernel_path: &Path,
    initrd_paths: &[RenderedInitrdCandidate<'_>],
    cmdline: &str,
    budget: &mut RenderBudget,
) -> Result<Box<[u8]>, ActiveReblitBlsRendererError> {
    let kernel_path = validated_relative_text(kernel_path);
    let mut length = "title ".len();
    length = add(length, display_name.len());
    length = add(length, " (".len());
    length = add(length, version.len());
    length = add(length, ")\n".len());
    length = add(length, "linux /".len());
    length = add(length, kernel_path.len());
    length = add(length, "\n\n".len());
    for initrd in initrd_paths {
        budget.step()?;
        length = add(length, "initrd /".len());
        length = add(length, validated_relative_text(&initrd.path).len());
        length = add(length, 1);
    }
    length = add(length, "options ".len());
    length = add(length, cmdline.len());
    length = add(length, 1);
    budget.admit_generated(entry_path, length)?;
    #[cfg(test)]
    record_materialization_start(entry_path);

    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|source| allocation("BLS Type 1 entry bytes", source))?;
    bytes.extend_from_slice(b"title ");
    bytes.extend_from_slice(display_name.as_bytes());
    bytes.extend_from_slice(b" (");
    bytes.extend_from_slice(version.as_bytes());
    bytes.extend_from_slice(b")\nlinux /");
    bytes.extend_from_slice(kernel_path.as_bytes());
    bytes.extend_from_slice(b"\n\n");
    for initrd in initrd_paths {
        bytes.extend_from_slice(b"initrd /");
        bytes.extend_from_slice(validated_relative_text(&initrd.path).as_bytes());
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(b"options ");
    bytes.extend_from_slice(cmdline.as_bytes());
    bytes.push(b'\n');
    debug_assert_eq!(bytes.len(), length);
    budget.require_deadline("Type 1 entry materialization")?;
    Ok(bytes.into_boxed_slice())
}

fn validated_relative_text(path: &Path) -> &str {
    path.to_str()
        .expect("renderer-created publication paths are validated ASCII")
}

fn add(total: usize, amount: usize) -> usize {
    total.checked_add(amount).unwrap_or(usize::MAX)
}

#[cfg(test)]
fn record_materialization_start(path: &Path) {
    MATERIALIZATION_STARTS.with(|starts| starts.borrow_mut().push(path.to_owned()));
}

#[cfg(test)]
pub(super) fn reset_materialization_starts() {
    MATERIALIZATION_STARTS.with(|starts| starts.borrow_mut().clear());
}

#[cfg(test)]
pub(super) fn take_materialization_starts() -> Vec<PathBuf> {
    MATERIALIZATION_STARTS.with(|starts| std::mem::take(&mut *starts.borrow_mut()))
}
