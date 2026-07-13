// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Typed lowering context for standard package-v2 build steps.
//!
//! This boundary deliberately accepts concrete values. It does not know about
//! legacy actions, definition names, or the script parser.

use std::collections::BTreeMap;

use stone_recipe::{
    derivation::{StepPlan, StepPlan::Run},
    package::StepSpec,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallLayout {
    pub prefix: String,
    pub bindir: String,
    pub sbindir: String,
    pub includedir: String,
    pub libdir: String,
    pub libexecdir: String,
    pub datadir: String,
    pub mandir: String,
    pub infodir: String,
    pub localedir: String,
    pub sysconfdir: String,
    pub localstatedir: String,
    pub sharedstatedir: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildContext {
    pub package_name: String,
    pub work_dir: String,
    pub build_subdir: String,
    pub install_root: String,
    pub target_triple: String,
    pub build_platform: String,
    pub host_platform: String,
    pub jobs: u32,
    pub layout: InstallLayout,
    pub environment: BTreeMap<String, String>,
}

impl BuildContext {
    /// Lower one standard builder step to an argv-preserving frozen step.
    ///
    /// `Shell` is deliberately not handled here. `CargoEnvironment` contributes
    /// to [`Self::environment`] and therefore has no executable step of its own.
    pub fn resolve_standard_step(&self, step: &StepSpec) -> Option<StepPlan> {
        let run = |program: &str, args: Vec<String>, environment: BTreeMap<String, String>| Run {
            program: program.to_owned(),
            args,
            environment: self
                .environment
                .iter()
                .chain(&environment)
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            working_dir: self.work_dir.clone(),
        };
        let values = |items: &[&str]| -> Vec<String> { items.iter().map(|item| (*item).to_owned()).collect() };
        let jobs = self.jobs.to_string();

        Some(match step {
            StepSpec::Shell { .. } | StepSpec::CargoEnvironment => return None,
            StepSpec::CMakeConfigure { flags } => {
                let mut args = values(&[
                    "-G",
                    "Ninja",
                    "-B",
                    &self.build_subdir,
                    "-DCMAKE_VERBOSE_MAKEFILE=ON",
                    "-DCMAKE_C_FLAGS_RELEASE=-DNDEBUG",
                    "-DCMAKE_CXX_FLAGS_RELEASE=-DNDEBUG",
                    "-DCMAKE_Fortran_FLAGS_RELEASE=-DNDEBUG",
                    "-DCMAKE_BUILD_TYPE=Release",
                    "-DCMAKE_INSTALL_DO_STRIP=OFF",
                    "-DCMAKE_INSTALL_LIBDIR=lib",
                ]);
                args.extend([
                    format!("-DCMAKE_INSTALL_LIBEXECDIR={}", self.layout.libexecdir),
                    format!("-DCMAKE_INSTALL_PREFIX={}", self.layout.prefix),
                    format!(
                        "-DCMAKE_LIB_SUFFIX={}",
                        self.layout.libdir.strip_prefix("/usr/lib").unwrap_or_default()
                    ),
                ]);
                args.extend(flags.iter().cloned());
                run("cmake", args, BTreeMap::new())
            }
            StepSpec::CMakeBuild => run(
                "cmake",
                values(&["--build", &self.build_subdir, "--verbose", "--parallel", &jobs]),
                BTreeMap::new(),
            ),
            StepSpec::CMakeInstall => run(
                "cmake",
                values(&["--install", &self.build_subdir, "--verbose"]),
                BTreeMap::from([("DESTDIR".to_owned(), self.install_root.clone())]),
            ),
            StepSpec::CMakeTest => run(
                "ctest",
                values(&[
                    "--test-dir",
                    &self.build_subdir,
                    "--verbose",
                    "--parallel",
                    &jobs,
                    "--output-on-failure",
                    "--force-new-ctest-process",
                ]),
                BTreeMap::new(),
            ),
            StepSpec::MesonSetup { flags } => {
                let mut args = vec![
                    "setup".to_owned(),
                    "--buildtype=plain".to_owned(),
                    format!("--prefix={}", self.layout.prefix),
                    format!(
                        "--libdir={}",
                        self.layout.libdir.strip_prefix("/usr/").unwrap_or(&self.layout.libdir)
                    ),
                    format!("--bindir={}", self.layout.bindir),
                    format!("--sbindir={}", self.layout.sbindir),
                    format!(
                        "--libexecdir={}",
                        self.layout
                            .libexecdir
                            .strip_prefix("/usr/")
                            .unwrap_or(&self.layout.libexecdir)
                    ),
                    format!("--includedir={}", self.layout.includedir),
                    format!("--datadir={}", self.layout.datadir),
                    format!("--mandir={}", self.layout.mandir),
                    format!("--infodir={}", self.layout.infodir),
                    format!("--localedir={}", self.layout.localedir),
                    format!("--sysconfdir={}", self.layout.sysconfdir),
                    format!("--localstatedir={}", self.layout.localstatedir),
                    "--wrap-mode=nodownload".to_owned(),
                ];
                args.extend(flags.iter().cloned());
                args.push(self.build_subdir.clone());
                run("meson", args, BTreeMap::new())
            }
            StepSpec::MesonBuild => run(
                "meson",
                values(&["compile", "--verbose", "-j", &jobs, "-C", &self.build_subdir]),
                BTreeMap::new(),
            ),
            StepSpec::MesonInstall => run(
                "meson",
                values(&["install", "--no-rebuild", "-C", &self.build_subdir]),
                BTreeMap::from([("DESTDIR".to_owned(), self.install_root.clone())]),
            ),
            StepSpec::MesonTest => run(
                "meson",
                values(&[
                    "test",
                    "--no-rebuild",
                    "--print-errorlogs",
                    "--verbose",
                    "-j",
                    &jobs,
                    "-C",
                    &self.build_subdir,
                ]),
                BTreeMap::new(),
            ),
            StepSpec::CargoFetch => run("cargo", values(&["fetch", "-v", "--locked"]), BTreeMap::new()),
            StepSpec::CargoBuild { features } => {
                let mut args = values(&[
                    "build",
                    "-v",
                    "-j",
                    &jobs,
                    "--frozen",
                    "--target",
                    &self.target_triple,
                    "--release",
                ]);
                if !features.is_empty() {
                    args.extend(["--features".to_owned(), features.join(",")]);
                }
                run("cargo", args, BTreeMap::new())
            }
            StepSpec::CargoInstall { binaries } => {
                let binaries = if binaries.is_empty() {
                    vec![self.package_name.as_str()]
                } else {
                    binaries.iter().map(String::as_str).collect()
                };
                let mut args = values(&[
                    "-Dm00755",
                    "-t",
                    &format!("{}{}", self.install_root, self.layout.bindir),
                ]);
                args.extend(
                    binaries
                        .into_iter()
                        .map(|binary| format!("target/{}/release/{binary}", self.target_triple)),
                );
                run("install", args, BTreeMap::new())
            }
            StepSpec::CargoTest { features } => {
                let mut args = values(&[
                    "test",
                    "-v",
                    "-j",
                    &jobs,
                    "--frozen",
                    "--target",
                    &self.target_triple,
                    "--release",
                ]);
                if !features.is_empty() {
                    args.extend(["--features".to_owned(), features.join(",")]);
                }
                args.push("--workspace".to_owned());
                run("cargo", args, BTreeMap::new())
            }
            StepSpec::AutotoolsConfigure { flags } => {
                let mut args = vec![
                    "./configure".to_owned(),
                    format!("--prefix={}", self.layout.prefix),
                    format!("--bindir={}", self.layout.bindir),
                    format!("--sbindir={}", self.layout.sbindir),
                    format!("--build={}", self.build_platform),
                    format!("--host={}", self.host_platform),
                    format!("--libdir={}", self.layout.libdir),
                    format!("--mandir={}", self.layout.mandir),
                    format!("--infodir={}", self.layout.infodir),
                    format!("--datadir={}", self.layout.datadir),
                    format!("--sysconfdir={}", self.layout.sysconfdir),
                    format!("--localstatedir={}", self.layout.localstatedir),
                    format!("--sharedstatedir={}", self.layout.sharedstatedir),
                    format!("--libexecdir={}", self.layout.libexecdir),
                ];
                args.extend(flags.iter().cloned());
                run(
                    "/usr/bin/dash",
                    args,
                    BTreeMap::from([
                        ("CONFIG_SHELL".to_owned(), "/usr/bin/dash".to_owned()),
                        ("SHELL".to_owned(), "/usr/bin/dash".to_owned()),
                    ]),
                )
            }
            StepSpec::AutotoolsBuild => run("make", values(&["VERBOSE=1", "-j", &jobs]), BTreeMap::new()),
            StepSpec::AutotoolsInstall => run(
                "make",
                values(&["install", &format!("DESTDIR={}", self.install_root)]),
                BTreeMap::new(),
            ),
            StepSpec::AutotoolsTest => run("make", values(&["check"]), BTreeMap::new()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> BuildContext {
        let prefix = "/usr".to_owned();
        BuildContext {
            package_name: "example".to_owned(),
            work_dir: "/mason/build/x86_64/source".to_owned(),
            build_subdir: "aerynos-builddir".to_owned(),
            install_root: "/mason/install".to_owned(),
            target_triple: "x86_64-unknown-linux-gnu".to_owned(),
            build_platform: "x86_64-aerynos-linux".to_owned(),
            host_platform: "x86_64-aerynos-linux".to_owned(),
            jobs: 8,
            layout: InstallLayout {
                bindir: "/usr/bin".to_owned(),
                sbindir: "/usr/sbin".to_owned(),
                includedir: "/usr/include".to_owned(),
                libdir: "/usr/lib".to_owned(),
                libexecdir: "/usr/lib/example".to_owned(),
                datadir: "/usr/share".to_owned(),
                mandir: "/usr/share/man".to_owned(),
                infodir: "/usr/share/info".to_owned(),
                localedir: "/usr/share/locale".to_owned(),
                sysconfdir: "/etc".to_owned(),
                localstatedir: "/var".to_owned(),
                sharedstatedir: "/var/lib".to_owned(),
                prefix,
            },
            environment: BTreeMap::from([("SOURCE_DATE_EPOCH".to_owned(), "1700000000".to_owned())]),
        }
    }

    #[test]
    fn cmake_and_meson_are_argv_preserving_run_steps() {
        let context = context();
        let Run {
            program,
            args,
            working_dir,
            ..
        } = context
            .resolve_standard_step(&StepSpec::CMakeConfigure {
                flags: vec!["-DBUILD_TESTS=OFF".to_owned()],
            })
            .unwrap()
        else {
            panic!("expected run")
        };
        assert_eq!(program, "cmake");
        assert_eq!(working_dir, context.work_dir);
        assert!(args.contains(&"-DBUILD_TESTS=OFF".to_owned()));
        assert!(args.windows(2).any(|values| values == ["-B", "aerynos-builddir"]));

        let Run { program, args, .. } = context.resolve_standard_step(&StepSpec::MesonBuild).unwrap() else {
            panic!("expected run")
        };
        assert_eq!(program, "meson");
        assert_eq!(args, ["compile", "--verbose", "-j", "8", "-C", "aerynos-builddir"]);
    }

    #[test]
    fn cargo_and_autotools_resolve_context_without_templates() {
        let context = context();
        let Run { program, args, .. } = context
            .resolve_standard_step(&StepSpec::CargoBuild {
                features: vec!["cli".to_owned(), "tls".to_owned()],
            })
            .unwrap()
        else {
            panic!("expected run")
        };
        assert_eq!(program, "cargo");
        assert!(
            args.windows(2)
                .any(|values| values == ["--target", "x86_64-unknown-linux-gnu"])
        );
        assert!(args.windows(2).any(|values| values == ["--features", "cli,tls"]));

        let Run {
            program,
            args,
            environment,
            ..
        } = context
            .resolve_standard_step(&StepSpec::AutotoolsConfigure { flags: Vec::new() })
            .unwrap()
        else {
            panic!("expected run")
        };
        assert_eq!(program, "/usr/bin/dash");
        assert!(args.contains(&"--build=x86_64-aerynos-linux".to_owned()));
        assert_eq!(environment["CONFIG_SHELL"], "/usr/bin/dash");
    }

    #[test]
    fn shell_and_environment_markers_never_enter_standard_lowering() {
        let context = context();
        assert!(
            context
                .resolve_standard_step(&StepSpec::Shell {
                    script: "%literal".to_owned()
                })
                .is_none()
        );
        assert!(context.resolve_standard_step(&StepSpec::CargoEnvironment).is_none());
    }
}
