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
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
        self.store_dir.join("snapshots").join(prefix).join(format!("{}.json", key))
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
            std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("serialização falhou: {}", e))
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
                                if let Some(stem) = name.to_str().and_then(|s| s.strip_suffix(".json")) {
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
pub fn snapshot_key(
    module_name: &str,
    source_hash: &str,
    lean_version: &str,
) -> String {
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
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = cmd.output()?;
    let status = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok((status, stdout, stderr))
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
