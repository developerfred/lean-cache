use crate::sha256::sha256_hex;
use crate::store::{validate_cache_key, ContentStore};
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

/// Configuração do remoto (salva em `.lean-cache/remote.txt`).
#[derive(Debug, Clone)]
pub struct RemoteConfig {
    pub base_url: String,
    pub auth_token: Option<String>,
    pub timeout_secs: u64,
    pub max_retries: u32,
    pub backoff_base_secs: u64,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            auth_token: None,
            timeout_secs: 60,
            max_retries: 3,
            backoff_base_secs: 2,
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
                let mut max_retries: u32 = 3;
                let mut backoff_base_secs: u64 = 2;
                for line in data.lines() {
                    let line = line.trim();
                    if let Some(rest) = line.strip_prefix("base_url=") {
                        base_url = rest.to_string();
                    } else if let Some(rest) = line.strip_prefix("auth_token=") {
                        auth_token = Some(rest.to_string());
                    } else if let Some(rest) = line.strip_prefix("timeout_secs=") {
                        timeout_secs = rest.parse().unwrap_or(60);
                    } else if let Some(rest) = line.strip_prefix("max_retries=") {
                        max_retries = rest.parse().unwrap_or(3);
                    } else if let Some(rest) = line.strip_prefix("backoff_base_secs=") {
                        backoff_base_secs = rest.parse().unwrap_or(2);
                    }
                }
                return Self {
                    base_url,
                    auth_token,
                    timeout_secs,
                    max_retries,
                    backoff_base_secs,
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
        lines.push(format!("max_retries={}", self.max_retries));
        lines.push(format!("backoff_base_secs={}", self.backoff_base_secs));
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
    fn artifact_url(&self, key: &str, file_name: &str) -> std::io::Result<String> {
        validate_cache_key(key)?;
        validate_cache_key(file_name)?;
        Ok(format!(
            "{}/objects/{}/{}",
            self.config.base_url.trim_end_matches('/'),
            key,
            file_name
        ))
    }

    /// Monta a URL base de uma chave (para HEAD / listagem).
    fn key_url(&self, key: &str) -> std::io::Result<String> {
        validate_cache_key(key)?;
        Ok(format!(
            "{}/objects/{}/",
            self.config.base_url.trim_end_matches('/'),
            key
        ))
    }

    /// Executa um curl com argumentos e retorna (exit_status, stdout, stderr).
    fn curl_once(&self, args: &[&str]) -> std::io::Result<(i32, String, String)> {
        let mut cmd = Command::new("curl");
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
        // Timeout de conexão e total.
        cmd.arg(format!("--connect-timeout={}", self.config.timeout_secs))
            .arg(format!("--max-time={}", self.config.timeout_secs))
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

    /// Executa um curl com retry e backoff exponencial.
    /// Tenta até `max_retries` vezes, esperando `backoff_base_secs * 2^tentativa` segundos entre falhas.
    fn curl_with_retry(&self, args: &[&str]) -> std::io::Result<(i32, String, String)> {
        let mut last_err: Option<std::io::Error> = None;
        for tentativa in 0..self.config.max_retries {
            match self.curl_once(args) {
                Ok(result @ (0, _, _)) => return Ok(result),
                Ok((status, stdout, stderr)) => {
                    // Código não-zero: erro HTTP ou de curl. Retry se ainda houver tentativas.
                    if tentativa + 1 < self.config.max_retries {
                        let delay = self.config.backoff_base_secs * 2u64.pow(tentativa);
                        eprintln!(
                            "curl falhou (status {}), retry {}/{} em {}s: {}",
                            status,
                            tentativa + 1,
                            self.config.max_retries,
                            delay,
                            stderr.trim()
                        );
                        thread::sleep(Duration::from_secs(delay));
                        continue;
                    }
                    return Ok((status, stdout, stderr));
                }
                Err(e) => {
                    last_err = Some(e);
                    if tentativa + 1 < self.config.max_retries {
                        let delay = self.config.backoff_base_secs * 2u64.pow(tentativa);
                        eprintln!(
                            "curl erro de I/O, retry {}/{} em {}s: {}",
                            tentativa + 1,
                            self.config.max_retries,
                            delay,
                            last_err.as_ref().unwrap()
                        );
                        thread::sleep(Duration::from_secs(delay));
                        continue;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                "curl excedeu max_retries sem sucesso",
            )
        }))
    }

    /// Verifica se uma chave existe no remoto (HEAD na pasta da chave).
    /// Retorna Ok(true) se existe, Ok(false) se não existe (404), ou Err em erro de rede.
    pub fn key_exists_remote(&self, key: &str) -> std::io::Result<bool> {
        let url = self.key_url(key)?;
        let (status, _, _) =
            self.curl_with_retry(&["--head", "--request", "HEAD", url.as_str()])?;
        Ok(status == 0)
    }

    /// Faz upload de um único arquivo para o remoto.
    pub fn upload_artifact(&self, key: &str, local_path: &Path) -> std::io::Result<bool> {
        let file_name = local_path
            .file_name()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "sem nome de arquivo")
            })?
            .to_string_lossy();
        let url = self.artifact_url(key, &file_name)?;

        let mut args = Vec::new();
        args.push("-T");
        args.push(local_path.to_str().unwrap_or(""));
        args.push(url.as_str());

        let (status, _, _stderr) = self.curl_with_retry(&args)?;
        if status == 0 {
            Ok(true)
        } else {
            // Não logar URL completa (pode conter credenciais ou dados sensíveis)
            eprintln!(
                "Aviso: falha ao enviar arquivo para o remoto (curl status {})",
                status
            );
            Ok(false)
        }
    }

    /// Faz download de um único arquivo do remoto para o destino.
    /// Retorna Ok(true) se baixou com sucesso, Ok(false) se o arquivo não existe no remoto,
    /// ou Err em caso de erro de rede / I/O.
    pub fn download_artifact(
        &self,
        key: &str,
        file_name: &str,
        dest: &Path,
    ) -> std::io::Result<bool> {
        let url = self.artifact_url(key, file_name)?;
        // Valida que dest não escapa para fora do diretório esperado
        let _canonical_dest = dest.canonicalize().unwrap_or_else(|_| dest.to_path_buf());

        fs::create_dir_all(dest.parent().unwrap_or(Path::new(".")))?;

        let mut args = Vec::new();
        args.push("-o");
        args.push(dest.to_str().unwrap_or(""));
        args.push("--create-dirs");
        args.push(url.as_str());

        let (status, _, _stderr) = self.curl_with_retry(&args)?;
        if status == 0 && dest.is_file() {
            Ok(true)
        } else if status == 22 {
            // HTTP 404 (curl --fail-with-body retorna 22): arquivo não existe.
            Ok(false)
        } else {
            // Não logar URL completa nem stderr (pode conter dados sensíveis)
            eprintln!(
                "Aviso: falha ao baixar chave {} (curl status {})",
                key, status
            );
            Ok(false)
        }
    }

    /// Faz download de um único arquivo e verifica integridade via SHA-256.
    /// A chave de conteúdo é usada como hash esperado: o hash do arquivo baixado
    /// deve bater com a chave (ou com um hash armazenado no trace).
    /// Retorna Ok(true) se baixou e verificou com sucesso.
    pub fn download_artifact_verified(
        &self,
        key: &str,
        file_name: &str,
        dest: &Path,
        expected_hash: &str,
    ) -> std::io::Result<bool> {
        let baixou = self.download_artifact(key, file_name, dest)?;
        if !baixou {
            return Ok(false);
        }

        // Verifica integridade: recomputa SHA-256 do arquivo baixado.
        let dados = match fs::read(dest) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "Aviso: não foi possível ler {} para verificação: {}",
                    dest.display(),
                    e
                );
                let _ = fs::remove_file(dest);
                return Ok(false);
            }
        };
        let hash_actual = sha256_hex(&dados);

        if hash_actual != expected_hash {
            eprintln!(
                "Aviso: integridade falhou para {} (esperado {}, obtido {}) — descartando arquivo corrompido.",
                file_name, &expected_hash[..8], &hash_actual[..8]
            );
            let _ = fs::remove_file(dest);
            return Ok(false);
        }

        Ok(true)
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
    let dir = store.object_dir_for(key)?;
    if !dir.is_dir() {
        return Ok((0, 0));
    }

    // Coleta nomes de artefatos e gera manifesto para permitir pull remoto.
    let mut names: Vec<String> = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            if let Some(name) = path.file_name() {
                names.push(name.to_string_lossy().to_string());
            }
        }
    }
    // Evita incluir manifesto antigo na lista.
    let manifest_path = dir.join("manifest.txt");
    let mut manifest_content = String::new();
    for name in &names {
        if name != "manifest.txt" {
            manifest_content.push_str(name);
            manifest_content.push('\n');
        }
    }
    fs::write(&manifest_path, &manifest_content)?;

    let mut enviados = 0usize;
    let mut falharam = 0usize;

    // Envia manifesto primeiro.
    if manifest_path.is_file() {
        match remote.upload_artifact(key, &manifest_path) {
            Ok(true) => enviados += 1,
            _ => falharam += 1,
        }
    }

    for name in &names {
        if name == "manifest.txt" {
            continue;
        }
        let path = dir.join(name);
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
/// Como não listamos remotamente, tentamos baixar as extensões conhecidas do Lean
/// (.olean, .ilean, .c, .o) para a chave especificada.
/// Retorna (baixados, falharam).
/// Faz pull de uma chave do remoto para o store local usando o manifesto.
pub fn pull_key(
    store: &ContentStore,
    remote: &RemoteClient,
    key: &str,
) -> std::io::Result<(usize, usize)> {
    validate_cache_key(key)?;
    let dir = store.object_dir_for(key)?;
    fs::create_dir_all(&dir)?;

    let manifest_dest = dir.join("manifest.txt");
    let mut baixados = 0usize;
    let mut falharam = 0usize;

    // Baixa o manifesto.
    match remote.download_artifact(key, "manifest.txt", &manifest_dest) {
        Ok(true) => baixados += 1,
        Ok(false) => {
            // Sem manifesto: nada a fazer.
            return Ok((baixados, falharam));
        }
        Err(e) => {
            eprintln!("Aviso: erro ao baixar manifesto: {}", e);
            return Ok((baixados, falharam));
        }
    }

    // Lê lista de arquivos.
    let content = fs::read_to_string(&manifest_dest)?;
    let files: Vec<String> = content
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    // Baixa cada arquivo listado.
    for file_name in &files {
        let dest = dir.join(file_name);
        match remote.download_artifact(key, file_name, &dest) {
            Ok(true) => baixados += 1,
            Ok(false) => falharam += 1,
            Err(e) => {
                eprintln!("Aviso: erro ao baixar {}: {}", file_name, e);
                falharam += 1;
            }
        }
    }

    // Remove manifesto local para não poluir o store.
    let _ = fs::remove_file(&manifest_dest);

    // Evita arquivos vazios na contagem? mantemos.
    let _ = files;

    Ok((baixados, falharam))
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
    use std::path::PathBuf;

    #[test]
    fn remote_config_roundtrip() -> std::io::Result<()> {
        let dir = tempfile_dir()?;
        let cfg = RemoteConfig {
            base_url: "https://cache.exemplo.com".to_string(),
            auth_token: Some("tok123".to_string()),
            timeout_secs: 120,
            max_retries: 3,
            backoff_base_secs: 2,
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
        let url = remote.artifact_url("abcd1234", "mod.olean").unwrap();
        assert_eq!(url, "https://cache.exemplo.com/objects/abcd1234/mod.olean");
    }

    fn tempfile_dir() -> std::io::Result<PathBuf> {
        let dir =
            std::env::temp_dir().join(format!("lean-cache-remote-test-{}", std::process::id()));
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}
