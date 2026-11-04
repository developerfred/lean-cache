#![allow(dead_code)]

/// Snapshots de estado de elaboração do `lean --server`.
///
/// O Lean 4 oferece um servidor de linguagem (`lean --server`) que mantém
/// o estado de elaboração na memória para edição interativa. Este módulo
/// permite salvar e restaurar snapshots desse estado entre sessões de build,
/// evitando re-elaboração do zero quando o estado já foi computado.
///
/// Isso é especialmente útil para:
/// - Builds incrementais no CI (restaurar snapshot do build anterior)
/// - Desenvolvimento interativo (reutilizar estado de elaboração entre edições)
/// - Workers remotos (compartilhar estado de elaboração entre máquinas)
use crate::sha256::sha256_hex;
use crate::store::validate_cache_key;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// Um snapshot do estado de elaboração do Lean para um módulo específico.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElaborationSnapshot {
    /// Nome do módulo (ex: `Testproj.Base`).
    pub module_name: String,
    /// Hash do conteúdo do fonte no momento do snapshot.
    pub source_hash: String,
    /// Versão do Lean usada.
    pub lean_version: String,
    /// Timestamp de criação do snapshot.
    pub created_at: DateTime<Utc>,
    /// Timestamp do último acesso.
    pub last_accessed: Option<DateTime<Utc>>,
    /// Dados serializados do estado de elaboração (formato específico do Lean).
    pub state_data: Vec<u8>,
    /// Chaves de declarações incluídas neste snapshot (para edição cirúrgica).
    pub declaration_keys: Vec<String>,
    /// Tamanho do snapshot em bytes.
    pub size_bytes: u64,
}

impl ElaborationSnapshot {
    /// Cria um novo snapshot a partir dos dados de estado do Lean.
    pub fn new(
        module_name: String,
        source_hash: String,
        lean_version: String,
        state_data: Vec<u8>,
        declaration_keys: Vec<String>,
    ) -> Self {
        let size_bytes = state_data.len() as u64;
        let now = Utc::now();
        Self {
            module_name,
            source_hash,
            lean_version,
            created_at: now,
            last_accessed: None,
            state_data,
            declaration_keys,
            size_bytes,
        }
    }

    /// Marca o snapshot como acessado agora.
    pub fn touch(&mut self) {
        self.last_accessed = Some(Utc::now());
    }

    /// Serializa o snapshot para JSON.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }

    /// Deserializa um snapshot de JSON.
    pub fn from_json(json: &str) -> serde_json::Result<Self> {
        serde_json::from_str(json)
    }
}

/// Store de snapshots de elaboração.
///
/// Estrutura: `<store_dir>/snapshots/<chave[0:2]>/<chave>.json`
#[derive(Debug)]
pub struct SnapshotStore {
    pub store_dir: PathBuf,
}

impl SnapshotStore {
    pub fn open(store_dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let store_dir = store_dir.into();
        let snapshots_root = store_dir.join("snapshots");
        fs::create_dir_all(&snapshots_root)?;
        Ok(Self { store_dir })
    }

    /// Caminho do arquivo para uma chave de snapshot.
    pub fn snapshot_path_for(&self, key: &str) -> PathBuf {
        validate_cache_key(key).expect("chave de snapshot inválida");
        let prefix = &key[..2.min(key.len())];
        self.store_dir
            .join("snapshots")
            .join(prefix)
            .join(format!("{}.json", key))
    }

    /// Retorna true se o snapshot já existe.
    pub fn contains(&self, key: &str) -> bool {
        self.snapshot_path_for(key).is_file()
    }

    /// Salva um snapshot no store.
    pub fn save_snapshot(&self, key: &str, snapshot: &ElaborationSnapshot) -> std::io::Result<()> {
        let path = self.snapshot_path_for(key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = snapshot.to_json().map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("serialização falhou: {}", e),
            )
        })?;
        fs::write(&path, json)?;
        Ok(())
    }

    /// Restaura um snapshot do store. Atualiza `last_accessed`.
    pub fn load_snapshot(&self, key: &str) -> std::io::Result<Option<ElaborationSnapshot>> {
        let path = self.snapshot_path_for(key);
        if path.is_file() {
            let data = fs::read_to_string(&path)?;
            let mut snapshot: ElaborationSnapshot = serde_json::from_str(&data)?;
            snapshot.touch();
            // Salva de volta com last_accessed atualizado
            let _ = self.save_snapshot(key, &snapshot);
            Ok(Some(snapshot))
        } else {
            Ok(None)
        }
    }

    /// Remove um snapshot do store.
    pub fn remove_snapshot(&self, key: &str) -> std::io::Result<bool> {
        let path = self.snapshot_path_for(key);
        if path.is_file() {
            fs::remove_file(&path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Lista todas as chaves de snapshot no store.
    pub fn list_keys(&self) -> std::io::Result<Vec<String>> {
        let mut keys = Vec::new();
        let snapshots_root = self.store_dir.join("snapshots");
        if snapshots_root.is_dir() {
            for prefix_entry in fs::read_dir(&snapshots_root)? {
                let prefix_entry = prefix_entry?;
                if prefix_entry.path().is_dir() {
                    for key_entry in fs::read_dir(prefix_entry.path())? {
                        let key_entry = key_entry?;
                        if key_entry.path().is_file() {
                            if let Some(name) = key_entry.path().file_name() {
                                if let Some(stem) =
                                    name.to_str().and_then(|s| s.strip_suffix(".json"))
                                {
                                    keys.push(stem.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        keys.sort();
        keys.dedup();
        Ok(keys)
    }

    /// Tamanho total do store de snapshots em bytes.
    pub fn size_bytes(&self) -> std::io::Result<u64> {
        let mut total = 0u64;
        let snapshots_root = self.store_dir.join("snapshots");
        if snapshots_root.is_dir() {
            for entry in fs::read_dir(&snapshots_root)? {
                let entry = entry?;
                if entry.path().is_file() {
                    if let Ok(meta) = entry.metadata() {
                        total += meta.len();
                    }
                }
            }
        }
        Ok(total)
    }
}

/// Calcula a chave de um snapshot baseado no módulo + hash do fonte + versão do Lean.
pub fn snapshot_key(module_name: &str, source_hash: &str, lean_version: &str) -> String {
    let input = format!("{}:{}:{}", module_name, source_hash, lean_version);
    sha256_hex(input.as_bytes())
}

/// Detecta se o `lean --server` está disponível no PATH.
pub fn find_lean_server() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            let p = Path::new(dir).join("lean");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Executa `lean --server` com um comando de snapshot (cria ou restaura).
/// Este é um wrapper fino; o protocolo exato do lean --server depende da versão.
pub fn run_lean_server_command(args: &[&str]) -> std::io::Result<(i32, String, String)> {
    let lean = find_lean_server().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "lean não encontrado no PATH")
    })?;
    let mut cmd = Command::new(&lean);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    let output = cmd.output()?;
    let status = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok((status, stdout, stderr))
}

/// Servidor Lean ativo para captura de estado de elaboração.
///
/// Gerencia um processo `lean --server` e se comunica via protocolo JSON-RPC
/// sobre stdin/stdout (formato Content-Length, semelhante ao LSP).
pub struct LeanServer {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: BufReader<ChildStdout>,
    next_id: u32,
}

impl LeanServer {
    /// Inicia um novo processo `lean --server`.
    pub fn new() -> std::io::Result<Self> {
        let lean = find_lean_server().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "lean não encontrado no PATH")
        })?;

        let mut child = Command::new(&lean)
            .arg("--server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdin = child.stdin.take().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                "não foi obter stdin do lean --server",
            )
        })?;
        let stdout = BufReader::new(child.stdout.take().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                "não foi obter stdout do lean --server",
            )
        })?);

        Ok(Self {
            child,
            stdin,
            stdout,
            next_id: 0,
        })
    }

    /// Envia uma requisição JSON-RPC e aguarda a resposta.
    fn send_request(&mut self, method: &str, params: Value) -> std::io::Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let body = serde_json::to_string(&request)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.stdin.write_all(header.as_bytes())?;
        self.stdin.write_all(body.as_bytes())?;
        self.stdin.flush()?;

        self.read_response(id)
    }

    /// Lê uma resposta JSON-RPC do stdout.
    fn read_response(&mut self, expected_id: u32) -> std::io::Result<Value> {
        loop {
            // Lê o cabeçalho Content-Length
            let mut content_length = None;
            let mut line = String::new();
            loop {
                line.clear();
                let bytes_read = self.stdout.read_line(&mut line)?;
                if bytes_read == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "lean --server fechou a conexão",
                    ));
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    // Linha vazia = fim do cabeçalho
                    break;
                }
                if let Some(len) = trimmed.strip_prefix("Content-Length: ") {
                    content_length = Some(len.trim().parse::<usize>().map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "Content-Length inválido",
                        )
                    })?);
                }
            }

            let len = content_length.ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Content-Length não encontrado no cabeçalho",
                )
            })?;

            // Lê o corpo JSON
            let mut body = vec![0u8; len];
            self.stdout.read_exact(&mut body)?;
            let response: Value = serde_json::from_slice(&body)?;

            let resp_id = response["id"].as_u64().unwrap_or(0);
            if resp_id == expected_id as u64 {
                if let Some(error) = response.get("error") {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("erro do lean --server: {}", error),
                    ));
                }
                return Ok(response["result"].clone());
            }
            // Resposta para outro ID, ignora e continua lendo.
        }
    }

    /// Captura o estado de elaboração de um arquivo Lean.
    /// Retorna o conteúdo do estado (ou None se não suportado).
    pub fn capture_elaboration_state(
        &mut self,
        file_path: &Path,
    ) -> std::io::Result<Option<Vec<u8>>> {
        let file_str = file_path.to_string_lossy().to_string();

        // Tenta usar o método 'textDocument/didOpen' para abrir o arquivo
        let _ = self.send_request(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": format!("file://{}", file_str),
                    "languageId": "lean",
                    "version": 1,
                    "text": std::fs::read_to_string(file_path).unwrap_or_default(),
                }
            }),
        );

        // Tenta obter o estado de elaboração via método específico do Lean
        match self.send_request(
            "lean/getElaborationState",
            json!({
                "file": file_str,
            }),
        ) {
            Ok(result) => {
                let state_json = serde_json::to_string(&result)?;
                Ok(Some(state_json.into_bytes()))
            }
            Err(e) => {
                // Se o método não for suportado, retorna None
                eprintln!(
                    "Aviso: lean --server não suporta getElaborationState: {}",
                    e
                );
                Ok(None)
            }
        }
    }

    /// Fecha o servidor Lean.
    pub fn close(mut self) -> std::io::Result<()> {
        let _ = self.send_request("shutdown", json!(null));
        let _ = self.send_request("exit", json!(null));
        self.child.wait()?;
        Ok(())
    }
}

impl Drop for LeanServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_roundtrip() {
        let snapshot = ElaborationSnapshot::new(
            "Testproj.Base".to_string(),
            "abc123".to_string(),
            "4.35.0".to_string(),
            vec![1, 2, 3, 4],
            vec!["baseNat".to_string(), "baseId".to_string()],
        );
        let json = snapshot.to_json().unwrap();
        let loaded = ElaborationSnapshot::from_json(&json).unwrap();
        assert_eq!(loaded.module_name, "Testproj.Base");
        assert_eq!(loaded.source_hash, "abc123");
        assert_eq!(loaded.lean_version, "4.35.0");
        assert_eq!(loaded.state_data, vec![1, 2, 3, 4]);
        assert_eq!(loaded.declaration_keys, vec!["baseNat", "baseId"]);
        assert_eq!(loaded.size_bytes, 4);
    }

    #[test]
    fn snapshot_store_roundtrip() -> std::io::Result<()> {
        let dir = tempfile_dir()?;
        let store = SnapshotStore::open(&dir)?;
        let key = "abcd1234";
        let snapshot = ElaborationSnapshot::new(
            "Testproj.Base".to_string(),
            "hash".to_string(),
            "4.35.0".to_string(),
            vec![0, 0],
            vec![],
        );
        store.save_snapshot(key, &snapshot)?;
        assert!(store.contains(key));
        let loaded = store.load_snapshot(key)?;
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().module_name, "Testproj.Base");
        Ok(())
    }

    fn tempfile_dir() -> std::io::Result<PathBuf> {
        let dir = std::env::temp_dir().join(format!("lean-cache-snapshot-{}", std::process::id()));
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}
