// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use snafu::{OptionExt, Snafu};
use std::collections::{BTreeMap, BTreeSet};

use crate::{KeyValue, Macros};

#[derive(Debug, Clone)]
pub enum Tuning {
    Enable,
    Disable,
    Config(String),
}

#[derive(Debug, Clone)]
pub struct TuningFlag {
    root: CompilerFlags,
    gnu: CompilerFlags,
    llvm: CompilerFlags,
}

impl TuningFlag {
    pub fn get(&self, flag: CompilerFlag, toolchain: Toolchain) -> Option<&str> {
        match toolchain {
            Toolchain::Llvm => self.llvm.get(flag),
            Toolchain::Gnu => self.gnu.get(flag),
        }
        .or_else(|| self.root.get(flag))
    }
}

#[derive(Debug, Clone, Copy)]
pub enum CompilerFlag {
    C,
    Cxx,
    F,
    D,
    Rust,
    Vala,
    Go,
    Ld,
}

#[derive(Debug, Clone, Default)]
pub struct CompilerFlags {
    c: Option<String>,
    cxx: Option<String>,
    f: Option<String>,
    d: Option<String>,
    rust: Option<String>,
    vala: Option<String>,
    go: Option<String>,
    ld: Option<String>,
}

/// Format-neutral compiler flag record used by declarative policy modules.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompilerFlagsSpec {
    pub c: Option<String>,
    pub cxx: Option<String>,
    pub f: Option<String>,
    pub d: Option<String>,
    pub rust: Option<String>,
    pub vala: Option<String>,
    pub go: Option<String>,
    pub ld: Option<String>,
}

/// Format-neutral root/toolchain-specific flag definition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TuningFlagSpec {
    pub root: CompilerFlagsSpec,
    pub gnu: CompilerFlagsSpec,
    pub llvm: CompilerFlagsSpec,
}

impl CompilerFlags {
    fn get(&self, flag: CompilerFlag) -> Option<&str> {
        match flag {
            CompilerFlag::C => self.c.as_deref(),
            CompilerFlag::Cxx => self.cxx.as_deref(),
            CompilerFlag::F => self.f.as_deref(),
            CompilerFlag::D => self.d.as_deref(),
            CompilerFlag::Rust => self.rust.as_deref(),
            CompilerFlag::Vala => self.vala.as_deref(),
            CompilerFlag::Go => self.go.as_deref(),
            CompilerFlag::Ld => self.ld.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub enum Toolchain {
    #[default]
    Llvm,
    Gnu,
}

#[derive(Debug, Clone)]
pub struct TuningOption {
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
}

/// Format-neutral set of flags enabled or disabled by a tuning choice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TuningOptionSpec {
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TuningGroup {
    pub root: TuningOption,
    pub default: Option<String>,
    pub choices: Vec<KeyValue<TuningOption>>,
}

/// Format-neutral tuning group and its named choices.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TuningGroupSpec {
    pub root: TuningOptionSpec,
    pub default: Option<String>,
    pub choices: Vec<crate::spec::KeyValueSpec<TuningOptionSpec>>,
}

impl From<CompilerFlagsSpec> for CompilerFlags {
    fn from(spec: CompilerFlagsSpec) -> Self {
        Self {
            c: spec.c,
            cxx: spec.cxx,
            f: spec.f,
            d: spec.d,
            rust: spec.rust,
            vala: spec.vala,
            go: spec.go,
            ld: spec.ld,
        }
    }
}

impl From<&CompilerFlags> for CompilerFlagsSpec {
    fn from(flags: &CompilerFlags) -> Self {
        Self {
            c: flags.c.clone(),
            cxx: flags.cxx.clone(),
            f: flags.f.clone(),
            d: flags.d.clone(),
            rust: flags.rust.clone(),
            vala: flags.vala.clone(),
            go: flags.go.clone(),
            ld: flags.ld.clone(),
        }
    }
}

impl From<TuningFlagSpec> for TuningFlag {
    fn from(spec: TuningFlagSpec) -> Self {
        Self {
            root: spec.root.into(),
            gnu: spec.gnu.into(),
            llvm: spec.llvm.into(),
        }
    }
}

impl From<&TuningFlag> for TuningFlagSpec {
    fn from(flag: &TuningFlag) -> Self {
        Self {
            root: (&flag.root).into(),
            gnu: (&flag.gnu).into(),
            llvm: (&flag.llvm).into(),
        }
    }
}

impl From<TuningOptionSpec> for TuningOption {
    fn from(spec: TuningOptionSpec) -> Self {
        Self {
            enabled: spec.enabled,
            disabled: spec.disabled,
        }
    }
}

impl From<&TuningOption> for TuningOptionSpec {
    fn from(option: &TuningOption) -> Self {
        Self {
            enabled: option.enabled.clone(),
            disabled: option.disabled.clone(),
        }
    }
}

impl From<TuningGroupSpec> for TuningGroup {
    fn from(spec: TuningGroupSpec) -> Self {
        Self {
            root: spec.root.into(),
            default: spec.default,
            choices: spec.choices.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<&TuningGroup> for TuningGroupSpec {
    fn from(group: &TuningGroup) -> Self {
        Self {
            root: (&group.root).into(),
            default: group.default.clone(),
            choices: group
                .choices
                .iter()
                .map(|choice| crate::spec::KeyValueSpec {
                    key: choice.key.clone(),
                    value: (&choice.value).into(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Default)]
pub struct Builder {
    flags: BTreeMap<String, TuningFlag>,
    groups: BTreeMap<String, TuningGroup>,
    enabled: BTreeSet<String>,
    disabled: BTreeSet<String>,
    option_sets: BTreeMap<String, String>,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_flag(&mut self, name: impl ToString, flag: TuningFlag) {
        self.flags.insert(name.to_string(), flag);
    }

    pub fn add_group(&mut self, name: impl ToString, group: TuningGroup) {
        self.groups.insert(name.to_string(), group);
    }

    pub fn add_macros(&mut self, macros: Macros) {
        for kv in macros.flags {
            self.add_flag(kv.key, kv.value);
        }
        for kv in macros.tuning {
            self.add_group(kv.key, kv.value);
        }
    }

    pub fn enable(&mut self, name: impl ToString, config: Option<String>) -> Result<(), Error> {
        let name = name.to_string();
        let group = self.groups.get(&name).context(UnknownGroupSnafu { name: &name })?;

        self.enabled.insert(name.clone());
        self.disabled.remove(&name);

        if let Some(value) = config.or_else(|| group.default.clone()) {
            snafu::ensure!(
                group.choices.iter().any(|kv| kv.key == value),
                UnknownGroupValueSnafu { value, group: name }
            );
            self.option_sets.insert(name, value);
        }

        Ok(())
    }

    pub fn disable(&mut self, name: impl ToString) -> Result<(), Error> {
        let name = name.to_string();
        snafu::ensure!(self.groups.contains_key(&name), UnknownGroupSnafu { name });

        self.disabled.insert(name.clone());
        self.enabled.remove(&name);
        self.option_sets.remove(&name);

        Ok(())
    }

    pub fn build(&self) -> Result<Vec<TuningFlag>, Error> {
        let mut enabled_flags = BTreeSet::new();
        let mut disabled_flags = BTreeSet::new();

        for enabled in &self.enabled {
            let Some(group) = self.groups.get(enabled) else {
                continue;
            };

            let mut to = &group.root;

            if let Some(option) = self.option_sets.get(enabled)
                && let Some(choice) = group.choices.iter().find(|kv| &kv.key == option)
            {
                to = &choice.value;
            }

            enabled_flags.extend(to.enabled.clone());
        }

        for disabled in &self.disabled {
            let Some(group) = self.groups.get(disabled) else {
                continue;
            };
            disabled_flags.extend(group.root.disabled.clone());
        }

        for flag in enabled_flags.iter().chain(&disabled_flags) {
            snafu::ensure!(self.flags.contains_key(flag), UnknownFlagSnafu { name: flag });
        }

        Ok(enabled_flags
            .iter()
            .chain(&disabled_flags)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|flag| self.flags.get(flag).cloned())
            .collect())
    }
}

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("unknown flag {name}"))]
    UnknownFlag { name: String },
    #[snafu(display("unknown group {name}"))]
    UnknownGroup { name: String },
    #[snafu(display("unknown value {value} for group {group}"))]
    UnknownGroupValue { value: String, group: String },
}
