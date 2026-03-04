use crate::error::{PyopsError, Result};
use crate::model::ConfigFile;
use crate::schedule::parse_restart_schedule;
use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn default_config_path() -> Result<PathBuf> {
    if let Ok(path) = env::var("PYM2_CONFIG") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
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
    for app in &cfg.apps {
        if app.command.is_empty() && !app.venv.trim().is_empty() && !app.entry.trim().is_empty() {
            eprintln!(
                "warning: app '{}' uses legacy venv/entry config; this is deprecated, prefer command=[...]",
                app.name
            );
        }
    }
    Ok(cfg)
}

pub fn save_config_to(path: &Path, cfg: &ConfigFile) -> Result<()> {
    validate_config(cfg)?;
    let content = toml::to_string_pretty(cfg).map_err(|e| {
        PyopsError::Config(format!(
            "failed to serialize config {}: {}",
            path.display(),
            e
        ))
    })?;
    write_atomic(path, content.as_bytes())
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
        if app.command.is_empty() {
            if app.venv.trim().is_empty() {
                return Err(PyopsError::Config(format!(
                    "app '{}' venv cannot be empty when command is not set",
                    app.name
                )));
            }
            if app.entry.trim().is_empty() {
                return Err(PyopsError::Config(format!(
                    "app '{}' is missing command and legacy fields; set command=[\"python\",\"-m\",\"uvicorn\",\"app.main:app\"] or provide venv+entry",
                    app.name
                )));
            }
        } else if app.command[0].trim().is_empty() {
            return Err(PyopsError::Config(format!(
                "app '{}' command executable cannot be empty",
                app.name
            )));
        }

        if let Some(env_file) = app.env_file.as_ref() {
            if env_file.trim().is_empty() {
                return Err(PyopsError::Config(format!(
                    "app '{}' env_file cannot be empty",
                    app.name
                )));
            }
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
                    "Refusing to bind Web UI to public interface without auth token.".to_string(),
                ));
            }
        }
    }

    Ok(())
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let tmp = path.with_extension("tmp");
    let mut f = File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
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
                command: Vec::new(),
                venv: ".venv".to_string(),
                entry: "app.main:app".to_string(),
                args: vec![],
                autostart: true,
                restart: RestartPolicy::OnFailure,
                stop_signal: "SIGTERM".to_string(),
                kill_timeout_ms: 8000,
                restart_schedule: None,
                env_file: None,
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
        assert!(err.to_string().contains("Refusing to bind Web UI"));
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
        assert!(err.to_string().contains("Refusing to bind Web UI"));
    }

    #[test]
    fn command_mode_does_not_require_legacy_fields() {
        let mut cfg = base_config();
        cfg.apps[0].command = vec![
            "python".to_string(),
            "-m".to_string(),
            "http.server".to_string(),
        ];
        cfg.apps[0].venv.clear();
        cfg.apps[0].entry.clear();

        validate_config(&cfg).expect("command mode should allow empty venv/entry");
    }

    #[test]
    fn save_and_load_roundtrip_command_mode() {
        let mut cfg = base_config();
        cfg.apps[0].command = vec![
            "python".to_string(),
            "-m".to_string(),
            "uvicorn".to_string(),
            "app.main:app".to_string(),
        ];
        cfg.apps[0].env_file = Some("/srv/api/.env".to_string());
        cfg.apps[0].venv.clear();
        cfg.apps[0].entry.clear();

        let path =
            std::env::temp_dir().join(format!("pym2-config-test-{}.toml", std::process::id()));
        let _ = fs::remove_file(&path);
        save_config_to(&path, &cfg).expect("save config");
        let loaded = load_config_from(&path).expect("load config");
        let _ = fs::remove_file(&path);

        assert_eq!(loaded.apps.len(), 1);
        assert_eq!(loaded.apps[0].command, cfg.apps[0].command);
        assert_eq!(loaded.apps[0].env_file, cfg.apps[0].env_file);
    }

    #[test]
    fn command_mode_rejects_empty_executable() {
        let mut cfg = base_config();
        cfg.apps[0].command = vec![" ".to_string(), "-m".to_string(), "uvicorn".to_string()];
        cfg.apps[0].venv.clear();
        cfg.apps[0].entry.clear();

        let err = validate_config(&cfg).expect_err("empty executable should fail");
        assert!(err.to_string().contains("command executable"));
    }

    #[test]
    fn rejects_empty_env_file_path() {
        let mut cfg = base_config();
        cfg.apps[0].env_file = Some(" ".to_string());

        let err = validate_config(&cfg).expect_err("empty env_file should fail");
        assert!(err.to_string().contains("env_file"));
    }
}
