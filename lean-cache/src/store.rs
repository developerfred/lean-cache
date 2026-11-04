
use crate::compression::{compress_file, decompress_file, CompressionAlgorithm};
use crate::sha256::{combine_hashes, sha256_hex};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Valida uma chave de cache para prevenir path traversal.
///
/// Regras:
/// - Não pode conter `..`
/// - Não pode começar com `/`
/// - Não pode conter byte nulo
/// - Não pode ser vazia
pub fn validate_cache_key(key: &str) -> std::io::Result<()> {
    if key.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "chave de cache não pode estar vazia",
        ));
    }
    if key.contains('\0') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "chave de cache não pode conter byte nulo",
        ));
    }
    if key.starts_with('/') || key.starts_with("./") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "chave de cache não pode começar com / ou ./",
        ));
    }
    if key.contains("..") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "chave de cache não pode conter path traversal (..)",
        ));
    }
    Ok(())
}

/// Artefatos produzidos por um módulo Lean (facetas do Lake).
pub const ARTIFACT_EXTENSIONS: &[&str] = &["olean", "ilean", "c", "o"];

/// Configuração do store de cache.
#[derive(Debug, Clone)]
pub struct CacheConfig {
    pub lean_version: Option<String>,
    pub default_flags: Vec<String>,
    pub store_dir: PathBuf,
    /// Tamanho mínimo (bytes) acima do qual um artefato é comprimido.
    /// Arquivos menores são armazenados cru para evitar overhead de compressão.
    pub compress_min_bytes: u64,
    /// Algoritmo de compressão usado para armazenar artefatos.
    pub compression: CompressionAlgorithm,
    /// Se verdadeiro, usa hard-link (quando disponível) em vez de cópia na restauração.
    pub hard_link_on_restore: bool,
    /// Quota máxima do store em bytes. 0 = sem limite.
    pub max_store_bytes: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            lean_version: None,
            default_flags: Vec::new(),
            store_dir: PathBuf::from(".lean-cache"),
            compress_min_bytes: 65536, // 64 KB: so comprime arquivos grandes
            compression: CompressionAlgorithm::best_available(CompressionAlgorithm::Zstd),
            hard_link_on_restore: true,
            max_store_bytes: 0,
        }
    }
}

/// O store endereçável por conteúdo. Artefatos são guardados em
/// `<store_dir>/objects/<chave[0:2]>/<chave>/`.
pub struct ContentStore {
    pub config: CacheConfig,
    pub objects_root: PathBuf,
}

impl ContentStore {
    pub fn open(store_dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let store_dir = store_dir.into();
        let objects_root = store_dir.join("objects");
        fs::create_dir_all(&objects_root)?;
        Ok(Self {
            config: CacheConfig {
                store_dir: store_dir.clone(),
                ..Default::default()
            },
            objects_root,
        })
    }

    pub fn with_config(config: CacheConfig) -> std::io::Result<Self> {
        fs::create_dir_all(&config.store_dir)?;
        let objects_root = config.store_dir.join("objects");
        fs::create_dir_all(&objects_root)?;
        Ok(Self {
            objects_root,
            config,
        })
    }

    /// Caminho da pasta de objetos para uma chave.
    /// Retorna erro se a chave contiver caracteres perigosos (path traversal).
    pub fn object_dir_for(&self, key: &str) -> std::io::Result<PathBuf> {
        validate_cache_key(key)?;
        let prefix = &key[..2.min(key.len())];
        Ok(self.objects_root.join(prefix).join(key))
    }

    /// Retorna true se a chave já possui artefatos no store.
    pub fn contains(&self, key: &str) -> bool {
        self.object_dir_for(key)
            .map(|d| d.is_dir())
            .unwrap_or(false)
    }

    /// Armazena um conjunto de artefatos (caminhos de arquivo) sob uma chave.
    /// Cada artefato é comprimido com o algoritmo configurado (se acima de compress_min_bytes)
    /// e salvo como `<object_dir>/<nome><ext>`. Arquivos pequenos são armazenados cru.
    pub fn store(&self, key: &str, artifacts: &[PathBuf]) -> std::io::Result<()> {
        let dir = self.object_dir_for(key)?;
        fs::create_dir_all(&dir)?;
        let use_compression = self.config.compression != CompressionAlgorithm::None;
        for artifact in artifacts {
            if artifact.is_file() {
                let file_name = artifact
                    .file_name()
                    .ok_or_else(|| {
                        std::io::Error::new(std::io::ErrorKind::InvalidInput, "sem nome de arquivo")
                    })?
                    .to_os_string();
                let meta = fs::metadata(artifact)?;
                let should_compress =
                    use_compression && meta.len() >= self.config.compress_min_bytes;
                let dest = if should_compress {
                    dir.join(format!(
                        "{}{}",
                        file_name.to_string_lossy(),
                        self.config.compression.extension()
                    ))
                } else {
                    dir.join(file_name)
                };
                if should_compress {
                    compress_file(artifact, &dest, self.config.compression)?;
                } else {
                    fs::copy(artifact, &dest)?;
                }
            }
        }
        Ok(())
    }

    /// Restaura todos os artefatos de uma chave para um diretório de destino.
    /// Retorna a lista de arquivos restaurados.
    /// Usa hard-link (quando disponível e habilitado) para evitar cópia de disco.
    pub fn restore(&self, key: &str, dest_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        let dir = self.object_dir_for(key)?;
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        // Sanitiza dest_dir para prevenir path traversal
        let dest_dir = dest_dir
            .canonicalize()
            .unwrap_or_else(|_| dest_dir.to_path_buf());
        fs::create_dir_all(&dest_dir)?;
        let mut restored = Vec::new();
        let compression = self.config.compression;
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                let file_name = path
                    .file_name()
                    .ok_or_else(|| {
                        std::io::Error::new(std::io::ErrorKind::InvalidInput, "sem nome de arquivo")
                    })?
                    .to_string_lossy()
                    .to_string();
                // Sanitiza nome de arquivo para prevenir path traversal
                let safe_name = file_name
                    .replace(['/', '\\'], "_")
                    .replace("..", "_");
                let name_str = safe_name.as_str();
                // Detecta se o arquivo está comprimido pela extensão do algoritmo configurado.
                if compression != CompressionAlgorithm::None
                    && name_str.ends_with(compression.extension())
                {
                    let original_name = name_str
                        .strip_suffix(compression.extension())
                        .unwrap_or(name_str);
                    let dest = dest_dir.join(original_name);
                    decompress_file(&path, &dest, compression)?;
                    restored.push(dest);
                } else if self.config.hard_link_on_restore {
                    // Tenta hard-link para evitar cópia de disco.
                    let dest = dest_dir.join(&safe_name);
                    if std::fs::hard_link(&path, &dest).is_ok() {
                        restored.push(dest);
                    } else {
                        // Fallback para cópia se hard-link não suportado (ex. FAT32, cross-device).
                        fs::copy(&path, &dest)?;
                        restored.push(dest);
                    }
                } else {
                    let dest = dest_dir.join(&safe_name);
                    fs::copy(&path, &dest)?;
                    restored.push(dest);
                }
            }
        }
        Ok(restored)
    }

    /// Remove uma chave do store.
    pub fn remove(&self, key: &str) -> std::io::Result<bool> {
        let dir = self.object_dir_for(key)?;
        if dir.is_dir() {
            fs::remove_dir_all(dir)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Lista todas as chaves no store.
    pub fn list_keys(&self) -> std::io::Result<Vec<String>> {
        let mut keys = Vec::new();
        if self.objects_root.is_dir() {
            for prefix_entry in fs::read_dir(&self.objects_root)? {
                let prefix_entry = prefix_entry?;
                if prefix_entry.path().is_dir() {
                    for key_entry in fs::read_dir(prefix_entry.path())? {
                        let key_entry = key_entry?;
                        if key_entry.path().is_dir() {
                            if let Some(name) = key_entry.path().file_name() {
                                keys.push(name.to_string_lossy().to_string());
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

    /// Tamanho aproximado do store em bytes.
    pub fn size_bytes(&self) -> std::io::Result<u64> {
        let mut total = 0u64;
        self.walk_objects(|path| {
            if let Ok(meta) = fs::metadata(path) {
                total += meta.len();
            }
        })?;
        Ok(total)
    }

    fn walk_objects<F>(&self, mut f: F) -> std::io::Result<()>
    where
        F: FnMut(&Path),
    {
        if self.objects_root.is_dir() {
            for prefix_entry in fs::read_dir(&self.objects_root)? {
                let prefix_entry = prefix_entry?;
                if prefix_entry.path().is_dir() {
                    for key_entry in fs::read_dir(prefix_entry.path())? {
                        let key_entry = key_entry?;
                        if key_entry.path().is_dir() {
                            for artifact in fs::read_dir(key_entry.path())? {
                                let artifact = artifact?;
                                let path = artifact.path();
                                if path.is_file() {
                                    f(&path);
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Executa garbage collection LRU: remove entradas menos recentemente acessadas
    /// quando o store excede max_store_bytes. Retorna (chaves_removidas, bytes_liberados).
    pub fn cache_gc(&self) -> std::io::Result<(usize, u64)> {
        let max_bytes = self.config.max_store_bytes;
        if max_bytes == 0 {
            return Ok((0, 0));
        }

        let current_size = self.size_bytes()?;
        if current_size <= max_bytes {
            return Ok((0, 0));
        }

        // Coleta todas as chaves com seus last_accessed_ns e tamanho.
        let mut entries: Vec<(String, Option<u128>, u64)> = Vec::new();
        for key in self.list_keys()? {
            let size = self.key_size_bytes(&key)?;
            let last_accessed = self.load_trace(&key)?.and_then(|t| t.last_accessed_ns);
            entries.push((key, last_accessed, size));
        }

        // Ordena por last_accessed_ns (None primeiro = nunca acessado = mais velho).
        entries.sort_by(|a, b| match (a.1, b.1) {
            (Some(na), Some(nb)) => na.cmp(&nb),
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });

        let mut removidas = 0usize;
        let mut liberados = 0u64;
        let mut accumulated = current_size;

        for (key, _last_accessed, size) in entries {
            if accumulated <= max_bytes {
                break;
            }
            if self.remove(&key)? {
                removidas += 1;
                liberados += size;
                accumulated = accumulated.saturating_sub(size);
            }
        }

        Ok((removidas, liberados))
    }

    /// Tamanho em bytes de uma chave específica.
    fn key_size_bytes(&self, key: &str) -> std::io::Result<u64> {
        let mut total = 0u64;
        let dir = self.object_dir_for(key)?;
        if dir.is_dir() {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                if let Ok(meta) = entry.metadata() {
                    total += meta.len();
                }
            }
        }
        Ok(total)
    }

    /// Salva o trace associado a uma chave (formato texto simples, linha a linha).
    /// Sanitiza source_file para não expor caminhos absolutos do sistema.
    pub fn save_trace(&self, key: &str, trace: &TraceRecord) -> std::io::Result<()> {
        let dir = self.object_dir_for(key)?;
        fs::create_dir_all(&dir)?;
        let mut lines = Vec::new();
        lines.push(format!("module:{}", trace.module));
        // Sanitiza source_file para evitar expor caminhos absolutos do sistema
        let safe_source = trace
            .source_file
            .replace(['/', '\\'], "_")
            .replace("..", "_");
        lines.push(format!("source_file:{}", safe_source));
        lines.push(format!("key:{}", trace.key));
        lines.push(format!(
            "lean_version:{}",
            trace.lean_version.clone().unwrap_or_default()
        ));
        lines.push(format!("flags:{}", trace.flags.join("\u{1f}")));
        lines.push(format!("imports:{}", trace.imports.join(",")));
        if let Some(ns) = trace.built_at_ns {
            lines.push(format!("built_at_ns:{}", ns));
        }
        if let Some(ns) = trace.last_accessed_ns {
            lines.push(format!("last_accessed_ns:{}", ns));
        }
        fs::write(dir.join("trace.txt"), lines.join("\n") + "\n")
    }

    /// Lê o trace de uma chave, se existir.
    pub fn load_trace(&self, key: &str) -> std::io::Result<Option<TraceRecord>> {
        let dir = self.object_dir_for(key)?;
        let path = dir.join("trace.txt");
        if path.is_file() {
            let data = fs::read_to_string(&path)?;
            Ok(Some(TraceRecord::from_text(&data)?))
        } else {
            Ok(None)
        }
    }
}

/// Registro de trace de um módulo Lean. O que determina se o módulo precisa ser recompilado.
#[derive(Debug, Clone)]
pub struct TraceRecord {
    pub module: String,
    pub source_file: String,
    pub key: String,
    pub lean_version: Option<String>,
    pub flags: Vec<String>,
    pub imports: Vec<String>,
    pub artifact_extensions: Vec<String>,
    pub built_at_ns: Option<u128>,
    pub last_accessed_ns: Option<u128>,
}

impl TraceRecord {
    pub fn from_text(text: &str) -> std::io::Result<Self> {
        let mut module = String::new();
        let mut source_file = String::new();
        let mut key = String::new();
        let mut lean_version: Option<String> = None;
        let mut flags: Vec<String> = Vec::new();
        let mut imports: Vec<String> = Vec::new();
        let mut built_at_ns: Option<u128> = None;
        let mut last_accessed_ns: Option<u128> = None;

        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("module:") {
                module = rest.to_string();
            } else if let Some(rest) = line.strip_prefix("source_file:") {
                source_file = rest.to_string();
            } else if let Some(rest) = line.strip_prefix("key:") {
                key = rest.to_string();
            } else if let Some(rest) = line.strip_prefix("lean_version:") {
                lean_version = if rest.is_empty() {
                    None
                } else {
                    Some(rest.to_string())
                };
            } else if let Some(rest) = line.strip_prefix("flags:") {
                if !rest.is_empty() {
                    flags = rest
                        .split('\u{1f}')
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .collect();
                }
            } else if let Some(rest) = line.strip_prefix("imports:") {
                if !rest.is_empty() {
                    imports = rest
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .collect();
                }
            } else if let Some(rest) = line.strip_prefix("built_at_ns:") {
                built_at_ns = rest.parse().ok();
            } else if let Some(rest) = line.strip_prefix("last_accessed_ns:") {
                last_accessed_ns = rest.parse().ok();
            }
        }
        Ok(Self {
            module,
            source_file,
            key,
            lean_version,
            flags,
            imports,
            artifact_extensions: ARTIFACT_EXTENSIONS.iter().map(|s| s.to_string()).collect(),
            built_at_ns,
            last_accessed_ns,
        })
    }
}

/// Constrói a chave de conteúdo de um módulo Lean.
/// Inputs: conteúdo do fonte, chaves dos módulos importados (transitivos),
/// versão do Lean e flags de compilação.
pub fn module_key(
    source_bytes: &[u8],
    import_keys: &[String],
    lean_version: &Option<String>,
    flags: &[String],
) -> String {
    let mut segments: Vec<&[u8]> = vec![source_bytes];
    for k in import_keys {
        segments.push(k.as_bytes());
    }
    if let Some(v) = lean_version {
        segments.push(b"lean-version:");
        segments.push(v.as_bytes());
    }
    for f in flags {
        segments.push(b"flag:");
        segments.push(f.as_bytes());
    }
    combine_hashes(&segments)
}

/// Hash simples do conteúdo de um arquivo para uso no trace.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let data = fs::read(path)?;
    Ok(sha256_hex(&data))
}

/// Resolve estaticamente o grafo de `import`s a partir de um arquivo raiz.
/// Retorna os módulos em ordem topológica (dependências antes do dependente).
///
/// Reconhece linhas como: `import Foo.Bar` (comentários após `--` são ignorados).
/// O caminho do módulo é resolvido substituindo `.` por `/` e adicionando `.lean`,
/// relativo ao diretório do arquivo que importa.
pub fn resolve_import_graph(
    root: &Path,
    roots: &[PathBuf],
) -> std::io::Result<Vec<(String, PathBuf, Vec<String>)>> {
    let root = fs::canonicalize(root)?;
    let mut visited: HashSet<String> = HashSet::new();
    let mut order: Vec<(String, PathBuf, Vec<String>)> = Vec::new();

    visit(&root, roots, &mut visited, &mut order)?;
    Ok(order)
}

fn visit(
    file: &Path,
    roots: &[PathBuf],
    visited: &mut HashSet<String>,
    order: &mut Vec<(String, PathBuf, Vec<String>)>,
) -> std::io::Result<()> {
    let canonical = fs::canonicalize(file)?;
    let module_name = module_name_from_path(&canonical, roots)?;
    if visited.contains(&module_name) {
        return Ok(());
    }
    visited.insert(module_name.clone());

    // Lê o fonte e extrai imports.
    let data = fs::read_to_string(file)?;
    let imports = extract_imports(&data);
    let file_dir = file.parent().unwrap_or(Path::new("."));

    // Resolve cada import para um caminho de arquivo.
    let mut import_paths: Vec<PathBuf> = Vec::new();
    for imp in &imports {
        if let Some(resolved) = resolve_module_path(imp, file_dir, roots)? {
            import_paths.push(resolved);
        }
    }

    // Visita dependências primeiro (ordem topológica).
    for imp_path in &import_paths {
        visit(imp_path, roots, visited, order)?;
    }

    order.push((module_name.clone(), canonical, imports));
    Ok(())
}

/// Nome do módulo a partir do caminho do arquivo, relativo à raiz do projeto.
/// Converte separadores de diretório em pontos (ex: Testproj/Base.lean -> Testproj.Base).
fn module_name_from_path(path: &Path, roots: &[PathBuf]) -> std::io::Result<String> {
    // Tenta calcular o caminho relativo a cada root.
    for root in roots {
        let canonical_root = fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        if let Ok(rel) = path.strip_prefix(&canonical_root) {
            let mut parts: Vec<String> = rel
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            // Remove a extensão .lean do último componente.
            if let Some(last) = parts.last_mut() {
                *last = last.strip_suffix(".lean").unwrap_or(last).to_string();
            }
            // Substitui pontos por underlines em cada componente.
            let parts: Vec<String> = parts.iter().map(|p| p.replace('.', "_")).collect();
            let name = parts.join(".");
            return Ok(name);
        }
    }
    // Fallback: usa o nome do arquivo sem extensão.
    let file_stem = path
        .file_stem()
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "sem nome de arquivo")
        })?
        .to_string_lossy();
    Ok(file_stem.replace(std::path::MAIN_SEPARATOR, "."))
}

/// Extrai nomes de módulos importados de um fonte Lean (linhas `import ...`).
fn extract_imports(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        // Ignora comentários de linha Lean (`--`) antes de processar.
        let line = line.split("--").next().unwrap_or(line).trim();
        if let Some(rest) = line.strip_prefix("import") {
            let rest = rest.trim();
            if rest.is_empty() {
                continue;
            }
            // O nome do módulo é o primeiro token até espaço/ponto-e-vírgula/chaves.
            let mut name = String::new();
            for ch in rest.chars() {
                if ch.is_whitespace() || ch == ';' || ch == '{' || ch == '}' {
                    break;
                }
                name.push(ch);
            }
            if !name.is_empty() {
                out.push(name);
            }
        }
    }
    out
}

/// Resolve o caminho de arquivo de um módulo importado.
fn resolve_module_path(
    module: &str,
    from_dir: &Path,
    roots: &[PathBuf],
) -> std::io::Result<Option<PathBuf>> {
    // Tenta relativo ao diretório do arquivo que importa.
    let mut relative_path = PathBuf::from(module);
    relative_path.set_extension("lean");
    // Converte pontos em separadores de diretório.
    let parts: Vec<&str> = module.split('.').collect();
    let mut p = PathBuf::new();
    for part in parts {
        p.push(part);
    }
    p.set_extension("lean");
    let candidate = from_dir.join(&p);
    if candidate.is_file() {
        return Ok(Some(candidate));
    }

    // Tenta em cada root de busca (-R).
    for root in roots {
        let candidate = root.join(&p);
        if candidate.is_file() {
            return Ok(Some(candidate));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_imports_basic() {
        let text = "import A.B\nimport C\n-- import D\nimport E.F.G  -- comentário\n";
        let imports = extract_imports(text);
        assert_eq!(
            imports,
            vec!["A.B".to_string(), "C".to_string(), "E.F.G".to_string()]
        );
    }

    #[test]
    fn sha256_combine_is_deterministic() {
        let k1 = module_key(b"foo", &["bar".to_string()], &None, &[]);
        let k2 = module_key(b"foo", &["bar".to_string()], &None, &[]);
        assert_eq!(k1, k2);
        let k3 = module_key(b"foo", &["bar".to_string()], &None, &["-O2".to_string()]);
        assert_ne!(k1, k3);
    }

    #[test]
    fn store_roundtrip() -> std::io::Result<()> {
        let dir = tempfile_dir()?;
        let store = ContentStore::open(&dir)?;
        let key = "abcd1234";
        let tmp = tempfile_dir()?;
        let artifact = tmp.join("mod.olean");
        fs::write(&artifact, b"fake-olean-content")?;
        store.store(key, &[artifact.clone()])?;
        assert!(store.contains(key));

        let out = tempfile_dir()?;
        let restored = store.restore(key, &out)?;
        assert_eq!(restored.len(), 1);
        let content = fs::read(out.join("mod.olean"))?;
        assert_eq!(content, b"fake-olean-content");
        Ok(())
    }

    #[test]
    fn trace_roundtrip() -> std::io::Result<()> {
        let dir = tempfile_dir()?;
        let store = ContentStore::open(&dir)?;
        let trace = TraceRecord {
            module: "Foo.Bar".to_string(),
            source_file: "src/Foo/Bar.lean".to_string(),
            key: "deadbeef".to_string(),
            lean_version: Some("4.8.0".to_string()),
            flags: vec!["-O2".to_string()],
            imports: vec!["Init".to_string()],
            artifact_extensions: ARTIFACT_EXTENSIONS.iter().map(|s| s.to_string()).collect(),
            built_at_ns: Some(12345),
            last_accessed_ns: Some(999),
        };
        store.save_trace("tracekey", &trace)?;
        let loaded = store.load_trace("tracekey")?.unwrap();
        assert_eq!(loaded.module, "Foo.Bar");
        assert_eq!(loaded.imports, vec!["Init".to_string()]);
        assert_eq!(loaded.flags, vec!["-O2".to_string()]);
        Ok(())
    }

    fn tempfile_dir() -> std::io::Result<PathBuf> {
        let dir = std::env::temp_dir().join(format!("lean-cache-test-{}", std::process::id()));
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}
