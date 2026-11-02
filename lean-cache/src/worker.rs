#![allow(dead_code)]

/// Workers remotos distribuidos para execucao de builds do Lean (estilo Bazel RE).
///
/// Este módulo implementa um cliente simples que envia tarefas de build para
/// workers remotos via HTTP (usando curl, consistente com o resto do projeto).
/// Suporta load balancing round-robin entre múltiplos workers.
///
/// Protocolo HTTP (implementado pelo servidor do worker, nao aqui):
///   POST /build   - envia { chave, fonte, imports } e recebe artefatos
///   GET  /keys    - lista chaves disponíveis no worker
///   GET  /status  - verifica se o worker está online
///   POST /store   - recebe artefatos para armazenar no worker

use crate::store::{validate_cache_key, ContentStore};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::process::{Command, Stdio};
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

/// Gera um nome de arquivo temporário não previsível para evitar
/// ataques de symlink/path traversal em diretorios temporarios compartilhados.
fn temp_filename_secure() -> String {
    let mut hasher = DefaultHasher::new();
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    format!("lean-cache-worker-{:x}", hasher.finish())
}

/// Configuração de um worker remoto.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub url: String,
    pub auth_token: Option<String>,
    pub timeout_secs: u64,
    pub max_retries: u32,
    pub weight: u32, // para load balancing ponderado (maior = mais tarefas)
}

/// Lista de workers remotos com load balancing round-robin.
#[derive(Debug, Clone)]
pub struct WorkerPool {
    workers: Vec<WorkerConfig>,
    current: usize,
}

impl WorkerPool {
    pub fn new() -> Self {
        Self {
            workers: Vec::new(),
            current: 0,
        }
    }

    pub fn add_worker(&mut self, worker: WorkerConfig) {
        self.workers.push(worker);
    }

    /// Retorna o próximo worker usando round-robin.
    pub fn next(&mut self) -> Option<&WorkerConfig> {
        if self.workers.is_empty() {
            return None;
        }
        let worker = &self.workers[self.current];
        self.current = (self.current + 1) % self.workers.len();
        Some(worker)
    }

    pub fn len(&self) -> usize {
        self.workers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.workers.is_empty()
    }
}

impl Default for WorkerPool {
    fn default() -> Self {
        Self::new()
    }
}

/// Requete de build enviada para um worker remoto.
#[derive(Serialize, Deserialize)]
pub struct BuildRequest {
    pub key: String,
    pub module_name: String,
    pub source_hash: String,
    pub import_keys: Vec<String>,
    pub lean_version: Option<String>,
    pub flags: Vec<String>,
    pub source_content: String, // conteúdo fonte em base64 ou texto puro
}

/// Resposta de build de um worker remoto.
#[derive(Serialize, Deserialize)]
pub struct BuildResponse {
    pub success: bool,
    pub artifacts: Vec<ArtifactInfo>,
    pub error: Option<String>,
}

/// Informação de um artefato retornado pelo worker.
#[derive(Serialize, Deserialize)]
pub struct ArtifactInfo {
    pub name: String,      // ex: "Mod1.olean"
    pub extension: String, // ex: "olean"
    pub content: String,   // conteúdo do artefato em base64
}

/// Cliente para se comunicar com workers remotos.
pub struct RemoteWorkerClient {
    config: WorkerConfig,
}

impl RemoteWorkerClient {
    pub fn new(config: WorkerConfig) -> Self {
        Self { config }
    }

    /// Cabeçalhos HTTP comuns.
    fn headers(&self) -> Vec<String> {
        let mut h = Vec::new();
        if let Some(t) = &self.config.auth_token {
            h.push(format!("Authorization: Bearer {}", t));
        }
        h.push("Content-Type: application/json".to_string());
        h
    }

    /// Executa um curl com argumentos e retorna (exit_status, stdout, stderr).
    fn curl(&self, args: &[&str]) -> std::io::Result<(i32, String, String)> {
        let mut cmd = Command::new("curl");
        cmd.args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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

    /// Verifica se o worker está online.
    pub fn is_online(&self) -> std::io::Result<bool> {
        let url = format!("{}/status", self.config.url.trim_end_matches('/'));
        let (status, _, _) = self.curl(&["--head", url.as_str()])?;
        Ok(status == 0)
    }

    /// Lista chaves disponíveis no worker.
    pub fn list_keys(&self) -> std::io::Result<Vec<String>> {
        let url = format!("{}/keys", self.config.url.trim_end_matches('/'));
        let (status, stdout, _) = self.curl(&[url.as_str()])?;
        if status != 0 {
            return Ok(Vec::new());
        }
        // Tenta fazer parse da resposta JSON
        let keys: Vec<String> = serde_json::from_str(&stdout).unwrap_or_default();
        Ok(keys)
    }

    /// Envia uma tarefa de build para o worker e aguarda o resultado.
    /// Se o worker retornar sucesso com artefatos, armazena no store local.
    pub fn request_build(
        &self,
        store: &ContentStore,
        req: &BuildRequest,
    ) -> std::io::Result<bool> {
        let url = format!("{}/build", self.config.url.trim_end_matches('/'));

        let json = serde_json::to_string(req).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("falha ao serializar requisicao: {}", e),
            )
        })?;

        let mut args = Vec::new();
        args.push("-X");
        args.push("POST");
        args.push("-d");
        args.push(json.as_str());
        args.push(url.as_str());

        let (status, stdout, stderr) = self.curl(&args)?;
        if status != 0 {
            eprintln!(
                "Worker {} indisponível (curl {}): {}",
                self.config.url, status, stderr.trim()
            );
            return Ok(false);
        }

        let resp: BuildResponse = serde_json::from_str(&stdout).unwrap_or(BuildResponse {
            success: false,
            artifacts: Vec::new(),
            error: Some("resposta inválida do worker".to_string()),
        });

        if !resp.success {
            eprintln!(
                "Worker {} reportou falha: {}",
                self.config.url,
                resp.error.unwrap_or_default()
            );
            return Ok(false);
        }

        // Armazena os artefatos recebidos no store local.
        let mut stored_any = false;
        for artifact in &resp.artifacts {
            // Decodifica o conteúdo base64.
            let data = match base64_decode(&artifact.content) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!(
                        "Aviso: falha ao decodificar artefato {} do worker: {}",
                        artifact.name, e
                    );
                    continue;
                }
            };

            // Grava em um arquivo temporário e armazena no cache.
            let tmp_dir = std::env::temp_dir().join(temp_filename_secure());
            fs::create_dir_all(&tmp_dir)?;
            let tmp_file = tmp_dir.join(&artifact.name);
            fs::write(&tmp_file, &data)?;

            // Valida a chave para prevenir path traversal
            validate_cache_key(&req.key)?;
            let key_dir = store.object_dir_for(&req.key);
            fs::create_dir_all(&key_dir)?;

            // Sanitiza nomes de arquivo para prevenir path traversal
            let safe_name = artifact.name.replace('/', "_").replace("..", "_");
            let safe_ext = artifact.extension.replace('/', "_").replace("..", "_");
            let dest_ext = if safe_ext.is_empty() {
                safe_name.clone()
            } else {
                format!("{}.{}", safe_name, safe_ext)
            };
            let dest = key_dir.join(dest_ext);

            fs::copy(&tmp_file, &dest)?;
            stored_any = true;

            // Limpa arquivo temporário.
            let _ = fs::remove_file(&tmp_file);
            let _ = fs::remove_dir_all(&tmp_dir);
        }

        Ok(stored_any)
    }

    /// Envia artefatos locais para um worker remoto (para replicação de cache).
    pub fn push_artifacts(
        &self,
        store: &ContentStore,
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
                let data = match fs::read(&path) {
                    Ok(d) => d,
                    Err(_) => {
                        falharam += 1;
                        continue;
                    }
                };

                let artifact = ArtifactInfo {
                    name: path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .to_string(),
                    extension: path
                        .extension()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string(),
                    content: base64_encode(&data),
                };

                let req = serde_json::json!({
                    "key": key,
                    "artifacts": [artifact]
                });

                let url = format!("{}/store", self.config.url.trim_end_matches('/'));
                let json = serde_json::to_string(&req).unwrap();

                let mut args = Vec::new();
                args.push("-X");
                args.push("POST");
                args.push("-d");
                args.push(json.as_str());
                args.push(url.as_str());

                let (status, _, stderr) = self.curl(&args)?;
                if status == 0 {
                    enviados += 1;
                } else {
                    eprintln!(
                        "Aviso: falha ao enviar {} para worker {} (curl {}): {}",
                        path.display(), self.config.url, status, stderr.trim()
                    );
                    falharam += 1;
                }
            }
        }

        Ok((enviados, falharam))
    }
}

/// Sincroniza o store local com todos os workers remotos (push de chaves locais).
pub fn sync_to_workers(
    store: &ContentStore,
    pool: &WorkerPool,
) -> std::io::Result<(usize, usize, usize)> {
    let keys = store.list_keys()?;
    let mut total_ok = 0usize;
    let mut total_fail = 0usize;
    let mut total_skipped = 0usize;

    // Para simplificar, usamos o primeiro worker disponível para cada chave.
    // Em uma implementação completa, distribuiria por round-robin.
    let mut pool = pool.clone();
    for key in &keys {
        if let Some(worker) = pool.next() {
            let client = RemoteWorkerClient::new(worker.clone());
            if client.is_online().unwrap_or(false) {
                // Verifica se a chave já existe no worker
                let worker_keys = client.list_keys().unwrap_or_default();
                if worker_keys.contains(key) {
                    total_skipped += 1;
                    continue;
                }
                let (ok, fail) = client.push_artifacts(store, key)?;
                total_ok += ok;
                total_fail += fail;
            } else {
                total_fail += 1;
            }
        }
    }

    Ok((total_ok, total_fail, total_skipped))
}

// ---------------------------------------------------------------------------
// Utilitários de base64 (implementação pura para evitar dependência extra)
// ---------------------------------------------------------------------------

const BASE64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8]) -> String {
    let mut result = String::new();
    let chunks = data.chunks(3);
    for chunk in chunks {
        match chunk.len() {
            3 => {
                result.push(BASE64_CHARS[(chunk[0] >> 2) as usize] as char);
                result.push(BASE64_CHARS[((chunk[0] & 0x03) << 4 | (chunk[1] >> 4)) as usize] as char);
                result.push(BASE64_CHARS[((chunk[1] & 0x0f) << 2 | (chunk[2] >> 6)) as usize] as char);
                result.push(BASE64_CHARS[(chunk[2] & 0x3f) as usize] as char);
            }
            2 => {
                result.push(BASE64_CHARS[(chunk[0] >> 2) as usize] as char);
                result.push(BASE64_CHARS[((chunk[0] & 0x03) << 4 | (chunk[1] >> 4)) as usize] as char);
                result.push(BASE64_CHARS[((chunk[1] & 0x0f) << 2) as usize] as char);
                result.push('=');
            }
            1 => {
                result.push(BASE64_CHARS[(chunk[0] >> 2) as usize] as char);
                result.push(BASE64_CHARS[((chunk[0] & 0x03) << 4) as usize] as char);
                result.push('=');
                result.push('=');
            }
            _ => {}
        }
    }
    result
}

pub fn base64_decode(input: &str) -> Result<Vec<u8>, Base64Error> {
    let mut result = Vec::new();
    let bytes: Vec<u8> = input.bytes().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' {
            break;
        }
        let b0 = pos(bytes[i])?;
        let b1 = if i + 1 < bytes.len() && bytes[i + 1] != b'=' {
            pos(bytes[i + 1])?
        } else {
            break;
        };
        let b2 = if i + 2 < bytes.len() && bytes[i + 2] != b'=' {
            pos(bytes[i + 2])?
        } else {
            break;
        };
        let b3 = if i + 3 < bytes.len() && bytes[i + 3] != b'=' {
            pos(bytes[i + 3])?
        } else {
            break;
        };

        result.push((b0 << 2) | (b1 >> 4));
        if i + 2 < bytes.len() && bytes[i + 2] != b'=' {
            result.push(((b1 & 0x0f) << 4) | (b2 >> 2));
        }
        if i + 3 < bytes.len() && bytes[i + 3] != b'=' {
            result.push(((b2 & 0x03) << 6) | b3);
        }
        i += 4;
    }
    Ok(result)
}

fn pos(b: u8) -> Result<u8, Base64Error> {
    match b {
        b'A'..=b'Z' => Ok(b - b'A'),
        b'a'..=b'z' => Ok(b - b'a' + 26),
        b'0'..=b'9' => Ok(b - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(Base64Error::InvalidCharacter(b)),
    }
}

#[derive(Debug)]
pub enum Base64Error {
    InvalidCharacter(u8),
}

impl std::fmt::Display for Base64Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Base64Error::InvalidCharacter(c) => {
                write!(f, "caractere base64 inválido: {}", *c as char)
            }
        }
    }
}

impl std::error::Error for Base64Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        let data = b"Hello, World!";
        let encoded = base64_encode(data);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn base64_empty() {
        let data = b"";
        let encoded = base64_encode(data);
        assert_eq!(encoded, "");
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn worker_pool_round_robin() {
        let mut pool = WorkerPool::new();
        pool.add_worker(WorkerConfig {
            url: "http://worker1".to_string(),
            auth_token: None,
            timeout_secs: 30,
            max_retries: 3,
            weight: 1,
        });
        pool.add_worker(WorkerConfig {
            url: "http://worker2".to_string(),
            auth_token: None,
            timeout_secs: 30,
            max_retries: 3,
            weight: 1,
        });
        assert_eq!(pool.len(), 2);
        let w1 = pool.next().unwrap().url.clone();
        let w2 = pool.next().unwrap().url.clone();
        let w3 = pool.next().unwrap().url.clone();
        assert_eq!(w1, "http://worker1");
        assert_eq!(w2, "http://worker2");
        assert_eq!(w3, "http://worker1"); // volta ao início
    }
}
