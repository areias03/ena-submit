//! Global configuration: credentials, environment default, and paths to the Webin-CLI toolchain.
//!
//! Config is loaded from `ena-submit.toml` in the current directory (if present) and then overlaid
//! with environment variables. Credentials should live in the environment (`WEBIN_USERNAME` /
//! `WEBIN_PASSWORD`), never in a committed file — the template `ena-submit.toml` leaves them blank.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::Environment;

/// Default config file name looked up in the working directory.
pub const CONFIG_FILE: &str = "ena-submit.toml";

/// Resolved configuration for a run.
#[derive(Debug, Clone)]
pub struct Config {
    /// Webin submission account username (e.g. `Webin-12345`), if known.
    pub webin_username: Option<String>,
    /// Webin submission account password, if known. Never serialized back out.
    pub webin_password: Option<String>,
    /// Default service to target when `--test` / `--production` is not given on the CLI.
    pub default_environment: Environment,
    /// Path to `webin-cli.jar`.
    pub webin_cli_jar: PathBuf,
    /// Java executable used to run the jar.
    pub java_bin: PathBuf,
    /// Directory Webin-CLI writes manifests, validation reports, and receipts into.
    pub output_dir: PathBuf,
}

/// On-disk representation of `ena-submit.toml`. All fields optional so a partial file is valid.
#[derive(Debug, Default, Deserialize, Serialize)]
struct ConfigFile {
    #[serde(default)]
    webin_username: Option<String>,
    #[serde(default)]
    webin_password: Option<String>,
    #[serde(default)]
    default_environment: Option<Environment>,
    #[serde(default)]
    webin_cli_jar: Option<PathBuf>,
    #[serde(default)]
    java_bin: Option<PathBuf>,
    #[serde(default)]
    output_dir: Option<PathBuf>,
}

impl Config {
    /// Load config from `ena-submit.toml` in `dir` (if present) then overlay environment variables.
    ///
    /// Precedence, low to high: built-in defaults < config file < environment.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(CONFIG_FILE);
        let file = if path.exists() {
            let text = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
            toml::from_str::<ConfigFile>(&text)
                .map_err(|source| Error::TomlParse { path, source })?
        } else {
            ConfigFile::default()
        };

        let env = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());

        Ok(Config {
            webin_username: env("WEBIN_USERNAME").or(file.webin_username),
            webin_password: env("WEBIN_PASSWORD").or(file.webin_password),
            default_environment: file.default_environment.unwrap_or_default(),
            webin_cli_jar: env("WEBIN_CLI_JAR")
                .map(PathBuf::from)
                .or(file.webin_cli_jar)
                .unwrap_or_else(|| PathBuf::from("webin-cli.jar")),
            java_bin: env("JAVA_BIN")
                .map(PathBuf::from)
                .or(file.java_bin)
                .unwrap_or_else(|| PathBuf::from("java")),
            output_dir: env("ENA_SUBMIT_OUTPUT_DIR")
                .map(PathBuf::from)
                .or(file.output_dir)
                .unwrap_or_else(|| PathBuf::from(".ena-submit/webin")),
        })
    }

    /// Return `(username, password)` or [`Error::MissingCredentials`] if either is absent.
    /// Call this only on paths that actually talk to Webin.
    pub fn require_credentials(&self) -> Result<(&str, &str)> {
        match (&self.webin_username, &self.webin_password) {
            (Some(u), Some(p)) => Ok((u.as_str(), p.as_str())),
            _ => Err(Error::MissingCredentials),
        }
    }
}

/// The template written by `ena-submit init`. Credentials are intentionally left blank.
pub const CONFIG_TEMPLATE: &str = "\
# ena-submit configuration.
#
# Credentials are best supplied via the environment so they never get committed:
#   export WEBIN_USERNAME=Webin-12345
#   export WEBIN_PASSWORD=...
# If you set them here instead, keep this file out of version control (see .gitignore).

# webin_username = \"Webin-12345\"
# webin_password = \"\"

# Which Webin service to target by default: \"test\" or \"production\".
default_environment = \"test\"

# Path to the Webin-CLI jar (download from
# https://github.com/enasequence/webin-cli/releases/latest). Requires Java 17+.
webin_cli_jar = \"webin-cli.jar\"

# Java executable used to run the jar.
java_bin = \"java\"

# Where Webin-CLI writes manifests, validation reports, and receipts.
output_dir = \".ena-submit/webin\"
";

#[cfg(test)]
mod tests {
    use super::*;

    /// A config with no credentials and all defaults, built without touching the process env.
    fn bare() -> Config {
        Config {
            webin_username: None,
            webin_password: None,
            default_environment: Environment::Test,
            webin_cli_jar: PathBuf::from("webin-cli.jar"),
            java_bin: PathBuf::from("java"),
            output_dir: PathBuf::from(".ena-submit/webin"),
        }
    }

    #[test]
    fn defaults_when_no_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load(dir.path()).unwrap();
        // Env-independent defaults (these env overrides are not set in a normal test run).
        assert_eq!(cfg.default_environment, Environment::Test);
        assert_eq!(cfg.java_bin, PathBuf::from("java"));
        assert_eq!(cfg.webin_cli_jar, PathBuf::from("webin-cli.jar"));
        assert_eq!(cfg.output_dir, PathBuf::from(".ena-submit/webin"));
    }

    #[test]
    fn loads_values_from_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            "default_environment = \"production\"\n\
             java_bin = \"/opt/java/bin/java\"\n\
             webin_cli_jar = \"/tools/webin-cli.jar\"\n",
        )
        .unwrap();

        let cfg = Config::load(dir.path()).unwrap();
        assert_eq!(cfg.default_environment, Environment::Production);
        assert_eq!(cfg.java_bin, PathBuf::from("/opt/java/bin/java"));
        assert_eq!(cfg.webin_cli_jar, PathBuf::from("/tools/webin-cli.jar"));
        // Unset key falls back to the built-in default.
        assert_eq!(cfg.output_dir, PathBuf::from(".ena-submit/webin"));
    }

    #[test]
    fn malformed_toml_is_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), "default_environment = ").unwrap();
        let err = Config::load(dir.path()).unwrap_err().to_string();
        assert!(err.contains("failed to parse TOML"), "got: {err}");
    }

    #[test]
    fn unknown_environment_value_is_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), "default_environment = \"staging\"\n").unwrap();
        assert!(Config::load(dir.path()).is_err());
    }

    #[test]
    fn require_credentials_reports_missing() {
        let cfg = bare();
        assert!(matches!(
            cfg.require_credentials(),
            Err(Error::MissingCredentials)
        ));
    }

    #[test]
    fn require_credentials_needs_both_halves() {
        let mut cfg = bare();
        cfg.webin_username = Some("Webin-12345".to_string());
        // Username without password is still incomplete.
        assert!(cfg.require_credentials().is_err());

        cfg.webin_password = Some("secret".to_string());
        assert_eq!(cfg.require_credentials().unwrap(), ("Webin-12345", "secret"));
    }
}
