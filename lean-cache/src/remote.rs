use crate::store::ContentStore;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Configuração do remoto (salva em `.lean-cache/remote.txt`).
#[derive(Debug, Clone)]
pub struct RemoteConfig {
    pub base_url: String,
    pub auth_token: Option<String>,
    pub timeout_secs: u64,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            auth_token: None,
            timeout_secs: 60,
        }
    }
}

impl RemoteConfig {
    pub fn load(store_dir: &Path) -> Self {
        let path = store_dir.join("remote.txt");
        if path.is_file() {
            if let Ok(data) = fs::read_to_string(&path) {
                let mut base_url = String::new();
                let mut auth_token: Option<String> = None;
                let mut timeout_secs: u64 = 60;
                for line in data.lines() {
                    let line = line.trim();
                    if let Some(rest) = line.strip_prefix("base_url=") {
                        base_url = rest.to_string();
                    } else if let Some(rest) = line.strip_prefix("auth_token=") {
                        auth_token = Some(rest.to_string());
                    } else if let Some(rest) = line.strip_prefix("timeout_secs=") {
                        timeout_secs = rest.parse().unwrap_or(60);
                    }
                }
                return Self {
                    base_url,
                    auth_token,
                    timeout_secs,
                };
            }
        }
        Self::default()
    }

    pub fn save(&self, store_dir: &Path) -> std::io::Result<()> {
        let path = store_dir.join("remote.txt");
        let mut lines = Vec::new();
        lines.push(format!("base_url={}", self.base_url));
        if let Some(t) = &self.auth_token {
            lines.push(format!("auth_token={}", t));
        }
        lines.push(format!("timeout_secs={}", self.timeout_secs));
        fs::write(path, lines.join("\n") + "\n")
    }
}

/// Cliente remoto mínimo. Usa `curl` via `std::process::Command` para não
/// depender de crates externos (compila 100% offline).
pub struct RemoteClient {
    pub config: RemoteConfig,
}

impl RemoteClient {
    pub fn new(config: RemoteConfig) -> Self {
        Self { config }
    }

    /// Cabeçalhos HTTP comuns (Authorization se houver token).
    fn headers(&self) -> Vec<String> {
        let mut h = Vec::new();
        if let Some(t) = &self.config.auth_token {
            h.push(format!("Authorization: Bearer {}", t));
        }
        h
    }

    /// Monta a URL para um artefato específico de uma chave.
    fn artifact_url(&self, key: &str, file_name: &str) -> String {
        format!(
            "{}/objects/{}/{}",
            self.config.base_url.trim_end_matches('/'),
            key,
            file_name
        )
    }

    /// Monta a URL base de uma chave (para HEAD / listagem).
    fn key_url(&self, key: &str) -> String {
        format!(
            "{}/objects/{}/",
            self.config.base_url.trim_end_matches('/'),
            key
        )
    }

    /// Executa um curl com argumentos e retorna (exit_status, stdout, stderr).
    fn curl(&self, args: &[&str]) -> std::io::Result<(i32, String, String)> {
        let mut cmd = Command::new("curl");
        cmd.args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Timeout de conexão e total.
        cmd.arg(format!(
            "--connect-timeout={}",
            self.config.timeout_secs
        ))
        .arg(format!(
            "--max-time={}",
            self.config.timeout_secs
        ))
        .arg("--fail-with-body")
        .arg("--silent")
        .arg("--show-error");

        for h in self.headers() {
            cmd.arg("-H").arg(h.as_str());
        }

        let output = cmd.output()?;
        let status = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Ok((status, stdout, stderr))
    }

    /// Verifica se uma chave existe no remoto (HEAD na primeira artefato conhecido
    /// ou na pasta da chave). Como não sabemos os nomes dos arquivos, tentamos HEAD
    /// na URL da chave; o servidor deve responder 200 se a chave existe.
    pub fn key_exists_remote(&self, key: &str) -> std::io::Result<bool> {
        let url = self.key_url(key);
        let (status, _, _) = self.curl(&[
            "--head",
            "--request",
            "HEAD",
            url.as_str(),
        ])?;
        Ok(status == 0)
    }

    /// Faz upload de um único arquivo para o remoto.
    pub fn upload_artifact(
        &self,
        key: &str,
        local_path: &Path,
    ) -> std::io::Result<bool> {
        let file_name = local_path
            .file_name()
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "sem nome de arquivo",
                )
            })?
            .to_string_lossy();
        let url = self.artifact_url(key, &file_name);

        let mut args = Vec::new();
        args.push("-T");
        args.push(local_path.to_str().unwrap_or(""));
        args.push(url.as_str());

        let (status, _, stderr) = self.curl(&args)?;
        if status == 0 {
            Ok(true)
        } else {
            eprintln!(
                "Aviso: falha ao enviar {} para {} (curl {}): {}",
                local_path.display(),
                url,
                status,
                stderr.trim()
            );
            Ok(false)
        }
    }

    /// Faz download de um único arquivo do remoto para o destino.
    pub fn download_artifact(
        &self,
        key: &str,
        file_name: &str,
        dest: &Path,
    ) -> std::io::Result<bool> {
        let url = self.artifact_url(key, file_name);
        fs::create_dir_all(dest.parent().unwrap_or(Path::new(".")))?;

        let mut args = Vec::new();
        args.push("-o");
        args.push(dest.to_str().unwrap_or(""));
        args.push("--create-dirs");
        args.push(url.as_str());

        let (status, _, stderr) = self.curl(&args)?;
        if status == 0 && dest.is_file() {
            Ok(true)
        } else {
            eprintln!(
                "Aviso: falha ao baixar {} de {} (curl {}): {}",
                url, key, status, stderr.trim()
            );
            Ok(false)
        }
    }

    /// Lista os arquivos de uma chave no remoto (tenta GET na pasta; depende do servidor).
    /// Retorna vazia se o servidor não suportar listagem.
    pub fn list_remote_artifacts(&self, _key: &str) -> std::io::Result<Vec<String>> {
        // Implementação mínima: sem listagem genérica. O caller deve saber os nomes.
        Ok(Vec::new())
    }
}

/// Faz push de todas as artefatos de uma chave local para o remoto.
/// Retorna (enviados, falharam).
pub fn push_key(
    store: &ContentStore,
    remote: &RemoteClient,
    key: &str,
) -> std::io::Result<(usize, usize)> {
    let dir = store.object_dir_for(key);
    if !dir.is_dir() {
        return Ok((0, 0));
    }

    let mut enviados = 0usize;
    let mut falharam = 0usize;

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            match remote.upload_artifact(key, &path) {
                Ok(true) => enviados += 1,
                _ => falharam += 1,
            }
        }
    }
    Ok((enviados, falharam))
}

/// Faz pull de uma chave do remoto para o store local.
/// Como não listamos remotamente, o caller deve passar os nomes de arquivo esperados.
/// Tentamos baixar as extensões conhecidas do Lean.
pub fn pull_key(
    _store: &ContentStore,
    _remote: &RemoteClient,
    _key: &str,
) -> std::io::Result<(usize, usize)> {
    // Implementação mínima: pull individual depende de listagem remota,
    // que ainda não está implementada. Use `sync` para push e `pull` (CLI)
    // para baixar uma chave específica.
    Ok((0, 0))
}

/// Sincroniza: faz push de todas as chaves locais que ainda não existem no remoto.
pub fn sync_all(
    store: &ContentStore,
    remote: &RemoteClient,
) -> std::io::Result<(usize, usize, usize)> {
    let keys = store.list_keys()?;
    let mut push_ok = 0usize;
    let mut push_fail = 0usize;
    let mut skipped = 0usize;

    for key in &keys {
        if remote.key_exists_remote(key)? {
            skipped += 1;
            continue;
        }
        let (ok, fail) = push_key(store, remote, key)?;
        push_ok += ok;
        push_fail += fail;
    }

    Ok((push_ok, push_fail, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn remote_config_roundtrip() -> std::io::Result<()> {
        let dir = tempfile_dir()?;
        let cfg = RemoteConfig {
            base_url: "https://cache.exemplo.com".to_string(),
            auth_token: Some("tok123".to_string()),
            timeout_secs: 120,
        };
        cfg.save(&dir)?;
        let loaded = RemoteConfig::load(&dir);
        assert_eq!(loaded.base_url, "https://cache.exemplo.com");
        assert_eq!(loaded.auth_token, Some("tok123".to_string()));
        assert_eq!(loaded.timeout_secs, 120);
        Ok(())
    }

    #[test]
    fn artifact_url_encoding() {
        let cfg = RemoteConfig {
            base_url: "https://cache.exemplo.com".to_string(),
            ..Default::default()
        };
        let remote = RemoteClient::new(cfg);
        let url = remote.artifact_url("abcd1234", "mod.olean");
        assert_eq!(url, "https://cache.exemplo.com/objects/abcd1234/mod.olean");
    }

    fn tempfile_dir() -> std::io::Result<PathBuf> {
        let dir = std::env::temp_dir().join(format!(
            "lean-cache-remote-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}
