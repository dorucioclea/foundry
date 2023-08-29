//! Generate [ethers-rs](https://github.com/gakonst/ethers-rs) bindings for solidity projects in a
//! build script.

#![allow(clippy::disallowed_macros)]

use crate::utils::{GitReference, GitRemote};
use ethers_contract::MultiAbigen;
pub use foundry_config::Config;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tempfile::{tempdir, TempDir};
use tracing::trace;
pub use url::Url;

pub mod utils;

/// Contains all the options to configure the gen process
#[derive(Debug)]
pub struct Binder {
    /// Where to find the project
    location: SourceLocation,
    /// Whether to include the bytecode in the bindings to be able to deploy them
    deployable: bool,
    /// Contains the directory where the artifacts should be written, if `None`, the artifacts will
    /// be cleaned up
    keep_artifacts: Option<PathBuf>,
    /// additional commands to run in the repo
    commands: Vec<Vec<String>>,
    /// The foundry config to use in order to compile the project
    config: Option<Config>,
    /// Path to where the contract artifacts are stored
    bindings: Option<PathBuf>,
}

// == impl Binder ==

impl Binder {
    /// Creates a new `Binder` instance for the given location
    ///
    /// # Example
    ///
    /// ## Local repository
    ///
    /// ```
    /// # use foundry_binder::Binder;
    /// # fn new() {
    ///  let binder = Binder::new("./aave-v3-core");
    /// # }
    /// ```
    ///
    /// ## Remote repository with default branch
    ///
    /// ```
    /// # use url::Url;
    /// use foundry_binder::Binder;
    /// # fn new() {
    ///  let binder = Binder::new(Url::parse("https://github.com/aave/aave-v3-core").unwrap());
    /// # }
    /// ```
    pub fn new(location: impl Into<SourceLocation>) -> Self {
        Self {
            location: location.into(),
            deployable: true,
            keep_artifacts: None,
            commands: vec![],
            config: None,
            bindings: None,
        }
    }

    /// Add a command to run in the project before generating the bindings
    ///
    /// # Example
    ///
    /// Add a `yarn install` command
    ///
    /// ```
    /// # use url::Url;
    /// use foundry_binder::{Binder, RepositoryBuilder};
    /// # fn new() {
    /// let binder = Binder::new(
    ///     RepositoryBuilder::new(Url::parse("https://github.com/aave/aave-v3-core").unwrap())
    ///         .tag("v1.16.0"),
    /// ).command(["yarn", "install"]);
    /// # }
    /// ```
    #[must_use]
    pub fn command<I, S>(mut self, cmd: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.commands.push(cmd.into_iter().map(Into::into).collect());
        self
    }

    /// If `deployable` set to `true` then the generated contract bindings will include the
    /// generated bytecode which makes the contracts deployable
    #[must_use]
    pub fn set_deployable(mut self, deployable: bool) -> Self {
        self.deployable = deployable;
        self
    }

    /// If set, the project's artifacts will be written there
    #[must_use]
    pub fn keep_artifacts(mut self, keep_artifacts: impl Into<PathBuf>) -> Self {
        self.keep_artifacts = Some(keep_artifacts.into());
        self
    }

    /// Sets the path where to write the bindings to
    #[must_use]
    pub fn bindings(mut self, bindings: impl Into<PathBuf>) -> Self {
        self.bindings = Some(bindings.into());
        self
    }

    /// Sets the config which contains all settings for how to compile the project
    ///
    /// ## Example
    ///
    /// ```
    /// # use url::Url;
    /// use foundry_binder::{Binder, Config, RepositoryBuilder};
    /// # fn new() {
    /// let binder = Binder::new(
    ///     RepositoryBuilder::new(Url::parse("https://github.com/aave/aave-v3-core").unwrap())
    ///         .tag("v1.16.0"),
    /// )
    /// .command(["yarn", "install"])
    /// .config(Config {
    ///     src: "src".into(),
    ///     out: "artifacts".into(),
    ///     ..Default::default()
    /// });
    /// # }
    /// ```
    #[must_use]
    pub fn config(mut self, config: Config) -> Self {
        self.config = Some(config);
        self
    }

    /// Generates the bindings
    pub fn generate(&self) -> eyre::Result<()> {
        let project = self.location.get()?;

        let config = if let Some(mut config) = self.config.clone() {
            config.__root = project.into();
            config
        } else {
            foundry_config::load_config_with_root(Some(project))
        };

        // run all commands
        for mut args in self.commands.clone() {
            eyre::ensure!(!args.is_empty(), "Command can't be empty");

            let mut cmd = Command::new(args.remove(0));
            cmd.current_dir(&config.__root.0)
                .args(args)
                .stderr(Stdio::inherit())
                .stdout(Stdio::inherit());
            trace!("Executing command {:?}", cmd);
            cmd.output()?;
        }

        let mut project = config.project()?;

        // overwrite the artifacts dir
        if let Some(keep_artifacts) = self.keep_artifacts.clone() {
            let _ = std::fs::create_dir_all(&keep_artifacts);
            project.paths.artifacts = keep_artifacts;
        }

        let compiled = project.compile()?;
        if compiled.has_compiler_errors() {
            eyre::bail!("Compiled with errors:\n{compiled}");
        }

        trace!("Generating bindings");
        let bindings = MultiAbigen::from_json_files(project.artifacts_path())?.build()?;
        trace!("Generated bindings");

        trace!("Writing bindings to `src/contracts`");
        let module = self.bindings.clone().unwrap_or_else(|| "src/contracts".into());
        bindings.write_to_module(module, false)?;

        Ok(())
    }
}

/// Where to find the source project
#[derive(Debug)]
pub enum SourceLocation {
    Local(PathBuf),
    Remote(Repository),
}

// === impl SourceLocation ===

impl SourceLocation {
    /// Returns the path to the project
    ///
    /// If this is a remote repository this will clone it
    pub fn get(&self) -> eyre::Result<PathBuf> {
        let path = match self {
            SourceLocation::Local(p) => p.clone(),
            SourceLocation::Remote(r) => {
                r.checkout()?;
                r.dest.as_ref().to_path_buf()
            }
        };
        Ok(path)
    }
}

impl From<Repository> for SourceLocation {
    fn from(repo: Repository) -> Self {
        SourceLocation::Remote(repo)
    }
}

impl From<RepositoryBuilder> for SourceLocation {
    fn from(builder: RepositoryBuilder) -> Self {
        SourceLocation::Remote(builder.build())
    }
}

impl From<Url> for SourceLocation {
    fn from(url: Url) -> Self {
        RepositoryBuilder::new(url).into()
    }
}

impl<'a> From<&'a str> for SourceLocation {
    fn from(path: &'a str) -> Self {
        SourceLocation::Local(path.into())
    }
}

impl<'a> From<&'a String> for SourceLocation {
    fn from(path: &'a String) -> Self {
        SourceLocation::Local(path.into())
    }
}

impl From<String> for SourceLocation {
    fn from(path: String) -> Self {
        SourceLocation::Local(path.into())
    }
}

#[derive(Debug)]
pub enum RepositoryDestination {
    Path(PathBuf),
    Temp(TempDir),
}

impl AsRef<Path> for RepositoryDestination {
    fn as_ref(&self) -> &Path {
        match self {
            RepositoryDestination::Path(p) => p,
            RepositoryDestination::Temp(dir) => dir.path(),
        }
    }
}

#[derive(Debug)]
pub struct Repository {
    /// github project repository like <https://github.com/aave/aave-v3-core/>
    pub repo: GitRemote,
    /// The version tag, branch or rev to checkout
    pub rev: GitReference,
    /// where to checkout the database
    pub db_path: Option<PathBuf>,
    /// Where to clone into
    pub dest: RepositoryDestination,
}

// === impl Repository ===

impl Repository {
    pub fn checkout(&self) -> eyre::Result<()> {
        fn copy_to(
            repo: &GitRemote,
            rev: &GitReference,
            db_path: &Path,
            dest: &Path,
        ) -> eyre::Result<()> {
            let (local, oid) = repo.checkout(db_path, rev, None)?;
            local.copy_to(oid, dest)?;
            Ok(())
        }

        if let Some(ref db) = self.db_path {
            copy_to(&self.repo, &self.rev, db, self.dest.as_ref())
        } else {
            let tmp = tempdir()?;
            let db = tmp.path().join(self.dest.as_ref().file_name().unwrap());
            copy_to(&self.repo, &self.rev, &db, self.dest.as_ref())
        }
    }
}

#[derive(Debug, Clone)]
#[must_use]
pub struct RepositoryBuilder {
    repo: GitRemote,
    rev: GitReference,
    dest: Option<PathBuf>,
    db_path: Option<PathBuf>,
}

// === impl RepositoryBuilder ===

impl RepositoryBuilder {
    pub fn new(url: Url) -> Self {
        Self { repo: GitRemote::new(url), rev: Default::default(), dest: None, db_path: None }
    }

    /// Specify the branch to checkout
    pub fn branch(mut self, branch: impl Into<String>) -> Self {
        self.rev = GitReference::Branch(branch.into());
        self
    }

    /// Specify the tag to checkout
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.rev = GitReference::Tag(tag.into());
        self
    }

    /// Specify the specific commit to checkout
    pub fn rev(mut self, rev: impl Into<String>) -> Self {
        self.rev = GitReference::Rev(rev.into());
        self
    }

    /// Specify a persistent location to clone into
    pub fn dest(mut self, dest: impl Into<PathBuf>) -> Self {
        self.dest = Some(dest.into());
        self
    }

    /// Sets the path to where to store the git database of the repo
    ///
    /// If None is provided a tempdir is used and the db is cleaned up after cloning
    pub fn database(mut self, db_path: impl Into<PathBuf>) -> Self {
        self.db_path = Some(db_path.into());
        self
    }

    pub fn build(self) -> Repository {
        let RepositoryBuilder { repo, rev, dest, db_path } = self;
        let dest = if let Some(dest) = dest {
            RepositoryDestination::Path(dest)
        } else {
            let name = repo.url().path_segments().unwrap().last().unwrap();
            let dir =
                tempfile::Builder::new().prefix(name).tempdir().expect("Failed to create tempdir");
            RepositoryDestination::Temp(dir)
        };
        Repository { dest, repo, rev, db_path }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn can_checkout_repo() {
        let _dest = "./assets/aave-v3-core";

        let repo =
            RepositoryBuilder::new("https://github.com/aave/aave-v3-core".parse().unwrap()).build();

        repo.checkout().unwrap();
    }
}
