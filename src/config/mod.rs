use crate::error::{PyopsError, Result};
use crate::model::ConfigFile;
use crate::schedule::parse_restart_schedule;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

pub fn default_config_path() -> Result<PathBuf> {
    Ok(PathBuf::from("/etc/pym2/config.toml"))
}

pub fn load_config() -> Result<ConfigFile> {
    let path = default_config_path()?;
    load_config_from(&path)
}

pub fn load_config_from(path: &Path) -> Result<ConfigFile> {
    let content = fs::read_to_string(path).map_err(|e| {
        PyopsError::Config(format!("failed to read config {}: {}", path.display(), e))
    })?;
    let cfg: ConfigFile = toml::from_str(&content)?;
    validate_config(&cfg)?;
    Ok(cfg)
}

pub fn load_config_or_defaults_for_client() -> Result<ConfigFile> {
    match load_config() {
        Ok(cfg) => Ok(cfg),
        Err(PyopsError::Config(_)) => Ok(ConfigFile {
            agent: Default::default(),
            apps: Vec::new(),
        }),
        Err(err) => Err(err),
    }
}

pub fn ensure_state_dirs(cfg: &ConfigFile) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let state_dir = expand_tilde(&cfg.agent.state_dir)?;
    let socket_path = expand_tilde(&cfg.agent.socket)?;
    let logs_dir = state_dir.join("logs");

    fs::create_dir_all(&state_dir)?;
    fs::create_dir_all(&logs_dir)?;
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)?;
    }

    Ok((state_dir, socket_path, logs_dir))
}

pub fn expand_tilde(input: &str) -> Result<PathBuf> {
    if !input.starts_with('~') {
        return Ok(PathBuf::from(input));
    }

    let home = env::var("HOME")
        .map_err(|_| PyopsError::Config("HOME is not set; cannot resolve '~' paths".to_string()))?;

    if input == "~" {
        return Ok(PathBuf::from(home));
    }

    let suffix = input
        .strip_prefix("~/")
        .ok_or_else(|| PyopsError::Config(format!("unsupported '~' form: {}", input)))?;
    Ok(PathBuf::from(home).join(suffix))
}

fn validate_config(cfg: &ConfigFile) -> Result<()> {
    for app in &cfg.apps {
        if app.name.trim().is_empty() {
            return Err(PyopsError::Config("app name cannot be empty".to_string()));
        }
        if app.cwd.trim().is_empty() {
            return Err(PyopsError::Config(format!(
                "app '{}' cwd cannot be empty",
                app.name
            )));
        }
        if app.venv.trim().is_empty() {
            return Err(PyopsError::Config(format!(
                "app '{}' venv cannot be empty",
                app.name
            )));
        }
        if app.entry.trim().is_empty() {
            return Err(PyopsError::Config(format!(
                "app '{}' entry cannot be empty",
                app.name
            )));
        }
        if let Some(schedule) = &app.restart_schedule {
            parse_restart_schedule(schedule)?;
        }
    }

    if cfg.agent.web.host.trim().is_empty() {
        return Err(PyopsError::Config(
            "agent.web.host cannot be empty".to_string(),
        ));
    }
    if cfg.agent.web.port == 0 {
        return Err(PyopsError::Config(
            "agent.web.port must be in range 1..65535".to_string(),
        ));
    }
    if cfg.agent.web.enabled && !is_loopback_host(cfg.agent.web.host.trim()) {
        match cfg.agent.web.password.as_ref().map(|p| p.trim()) {
            Some(password) if !password.is_empty() => {}
            _ => {
                return Err(PyopsError::Config(
                    "agent.web.password must be set when web is enabled on a non-loopback host"
                        .to_string(),
                ));
            }
        }
    }

    Ok(())
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentConfig, AppSpec, RestartPolicy, WebConfig};
    use std::collections::HashMap;

    fn base_config() -> ConfigFile {
        ConfigFile {
            agent: AgentConfig {
                socket: "/run/pym2/pym2.sock".to_string(),
                state_dir: "/var/lib/pym2".to_string(),
                web: WebConfig {
                    enabled: false,
                    host: "127.0.0.1".to_string(),
                    port: 17877,
                    password: None,
                },
            },
            apps: vec![AppSpec {
                name: "api".to_string(),
                cwd: "/srv/api".to_string(),
                venv: ".venv".to_string(),
                entry: "app.main:app".to_string(),
                args: vec![],
                autostart: true,
                restart: RestartPolicy::OnFailure,
                stop_signal: "SIGTERM".to_string(),
                kill_timeout_ms: 8000,
                restart_schedule: None,
                env: HashMap::new(),
            }],
        }
    }

    #[test]
    fn non_loopback_web_requires_password() {
        let mut cfg = base_config();
        cfg.agent.web.enabled = true;
        cfg.agent.web.host = "0.0.0.0".to_string();
        cfg.agent.web.password = None;

        let err = validate_config(&cfg).expect_err("config should fail without password");
        assert!(err.to_string().contains("agent.web.password"));
    }

    #[test]
    fn loopback_web_allows_missing_password() {
        let mut cfg = base_config();
        cfg.agent.web.enabled = true;
        cfg.agent.web.host = "127.0.0.1".to_string();
        cfg.agent.web.password = None;

        validate_config(&cfg).expect("loopback host should allow no password");
    }

    #[test]
    fn web_checks_run_even_when_no_apps() {
        let mut cfg = base_config();
        cfg.apps.clear();
        cfg.agent.web.enabled = true;
        cfg.agent.web.host = "0.0.0.0".to_string();
        cfg.agent.web.password = None;

        let err = validate_config(&cfg).expect_err("web checks must still run");
        assert!(err.to_string().contains("agent.web.password"));
    }
}
