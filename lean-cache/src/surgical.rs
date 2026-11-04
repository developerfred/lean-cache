
/// Edição cirúrgica (surgical editing) para o `lean-cache`.
///
/// Em vez de cachear um módulo inteiro como uma unidade atômica, este módulo
/// permite cachear **declarações individuais** (def, theorem, lemma, structure,
/// class, instance, etc.) de um arquivo Lean.
///
/// Quando um arquivo muda, apenas as declarações modificadas são recompiladas;
/// declarações inalteradas são reutilizadas do cache, evitando re-elaboração
/// desnecessária de todo o módulo.
///
/// Este é o nível mais fino de granularidade de cache para Lean 4 e traz o
/// maior ganho para edições locais em arquivos grandes.
use crate::sha256::sha256_hex;
use crate::store::validate_cache_key;
use std::fs;
use std::path::PathBuf;

/// Tipo de declaração Lean reconhecida.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DeclKind {
    Def,
    Theorem,
    Lemma,
    Structure,
    Class,
    Instance,
    Axiom,
    Constant,
    Variable,
    Other(String),
}

impl DeclKind {
    /// Classifica uma linha Lean com base na palavra-chave inicial.
    pub fn from_keyword(keyword: &str) -> Self {
        match keyword {
            "def" => DeclKind::Def,
            "theorem" => DeclKind::Theorem,
            "lemma" => DeclKind::Lemma,
            "structure" => DeclKind::Structure,
            "class" => DeclKind::Class,
            "instance" => DeclKind::Instance,
            "axiom" => DeclKind::Axiom,
            "constant" => DeclKind::Constant,
            "variable" => DeclKind::Variable,
            other => DeclKind::Other(other.to_string()),
        }
    }

    /// Retorna a representacao em string do tipo de declaracao.
    pub fn kind_as_str(&self) -> &str {
        match self {
            DeclKind::Def => "def",
            DeclKind::Theorem => "theorem",
            DeclKind::Lemma => "lemma",
            DeclKind::Structure => "structure",
            DeclKind::Class => "class",
            DeclKind::Instance => "instance",
            DeclKind::Axiom => "axiom",
            DeclKind::Constant => "constant",
            DeclKind::Variable => "variable",
            DeclKind::Other(s) => s,
        }
    }
}

/// Uma declaração Lean extraída de um arquivo fonte.
#[derive(Debug, Clone)]
pub struct LeanDeclaration {
    /// Nome da declaração (ex: `baseNat`, `mod1Chain`).
    pub name: String,
    /// Tipo da declaração (def, theorem, etc.).
    pub kind: DeclKind,
    /// Número da linha onde começa (1-based).
    pub start_line: usize,
    /// Número da linha onde termina (1-based, inclusivo).
    pub end_line: usize,
    /// Conteúdo bruto da declaração (para hash).
    pub content: String,
    /// Hash SHA-256 do conteúdo desta declaração.
    pub content_hash: String,
    /// Nomes de declarações que esta declaração importa/depende.
    pub depends_on: Vec<String>,
}

impl LeanDeclaration {
    /// Calcula a chave de conteúdo para esta declaração.
    pub fn key(&self) -> String {
        sha256_hex(self.content.as_bytes())
    }
}

/// Extrai declarações de um arquivo fonte Lean.
///
/// Reconhece palavras-chave: def, theorem, lemma, structure, class, instance,
/// axiom, constant, variable.
///
/// Uma declaração termina quando encontramos outra palavra-chave de declaração
/// no mesmo nível de indentação (coluna 0) ou o final do arquivo.
pub fn extract_declarations(source: &str) -> Vec<LeanDeclaration> {
    let mut decls: Vec<LeanDeclaration> = Vec::new();
    let lines: Vec<&str> = source.lines().collect();

    let mut current: Option<LeanDeclaration> = None;

    for (i, line) in lines.iter().enumerate() {
        let line_num = i + 1;
        let trimmed = line.trim();

        // Ignora linhas vazias e comentários de linha.
        if trimmed.is_empty() || trimmed.starts_with("--") {
            if let Some(ref mut _decl) = current {
                // Se a linha está vazia ou é comentário, pode fazer parte da declaração atual
                // apenas se já tivermos começado. Não finalizamos ainda.
            }
            continue;
        }

        // Verifica se a linha começa com uma palavra-chave de declaração na coluna 0.
        if line.starts_with(|c: char| c.is_whitespace()) {
            // Linha indentada: provavelmente continua a declaração anterior.
            if let Some(ref mut _decl) = current {
                _decl.content.push_str(line);
                _decl.content.push('\n');
                _decl.end_line = line_num;
            }
            continue;
        }

        // Linha na coluna 0: pode ser início de uma nova declaração.
        let first_word = trimmed.split_whitespace().next().unwrap_or("");

        // Se tivermos uma declaração em andamento, finalizamos ela.
        if let Some(decl) = current.take() {
            decls.push(decl);
        }

        // Verifica se é uma palavra-chave de declaração.
        let keyword = first_word;
        if matches!(
            keyword,
            "def"
                | "theorem"
                | "lemma"
                | "structure"
                | "class"
                | "instance"
                | "axiom"
                | "constant"
                | "variable"
        ) {
            // Extrai o nome da declaração (segundo token).
            let mut tokens = trimmed.split_whitespace();
            tokens.next(); // pula a palavra-chave
            let name = tokens.next().unwrap_or("unknown").to_string();

            let mut content = String::new();
            content.push_str(line);
            content.push('\n');

            current = Some(LeanDeclaration {
                name,
                kind: DeclKind::from_keyword(keyword),
                start_line: line_num,
                end_line: line_num,
                content,
                content_hash: String::new(), // será calculado abaixo
                depends_on: Vec::new(),
            });
        } else {
            // Linha na coluna 0 que não é declaração (ex: import, open, etc.)
            // Ignoramos para fins de extração de declarações.
        }
    }

    // Finaliza a última declaração se houver.
    if let Some(decl) = current {
        decls.push(decl);
    }

    // Calcula os hashes de conteúdo para cada declaração.
    for decl in &mut decls {
        decl.content_hash = sha256_hex(decl.content.as_bytes());
    }

    decls
}

/// Chave de conteúdo cirúrgica para uma declaração.
/// Inclui o conteúdo da declaração + chaves das declarações de que depende.
pub fn surgical_decl_key(
    decl_content: &str,
    dep_keys: &[String],
    lean_version: &Option<String>,
) -> String {
    let mut segments: Vec<&[u8]> = vec![decl_content.as_bytes()];
    for k in dep_keys {
        segments.push(k.as_bytes());
    }
    if let Some(v) = lean_version {
        segments.push(b"lean-version:");
        segments.push(v.as_bytes());
    }
    crate::sha256::combine_hashes(&segments)
}

/// Store cirúrgico: armazena declarações individuais endereçáveis por conteúdo.
///
/// Estrutura: `<store_dir>/surgical/<chave[0:2]>/<chave>/`
#[derive(Debug)]
pub struct SurgicalStore {
    pub store_dir: PathBuf,
}

impl SurgicalStore {
    pub fn open(store_dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let store_dir = store_dir.into();
        let surgical_root = store_dir.join("surgical");
        fs::create_dir_all(&surgical_root)?;
        Ok(Self { store_dir })
    }

    /// Caminho da pasta para uma chave de declaração.
    pub fn decl_dir_for(&self, key: &str) -> PathBuf {
        validate_cache_key(key).expect("chave de declaração inválida");
        let prefix = &key[..2.min(key.len())];
        self.store_dir.join("surgical").join(prefix).join(key)
    }

    /// Retorna true se a declaração já está em cache.
    pub fn contains(&self, key: &str) -> bool {
        self.decl_dir_for(key).is_dir()
    }

    /// Armazena o conteúdo de uma declaração sob sua chave.
    pub fn store_decl(&self, key: &str, content: &[u8]) -> std::io::Result<()> {
        let dir = self.decl_dir_for(key);
        fs::create_dir_all(&dir)?;
        let dest = dir.join("decl.lean");
        fs::write(&dest, content)?;
        Ok(())
    }

    /// Restaura o conteúdo de uma declaração do cache.
    pub fn restore_decl(&self, key: &str) -> std::io::Result<Option<Vec<u8>>> {
        let dir = self.decl_dir_for(key);
        let dest = dir.join("decl.lean");
        if dest.is_file() {
            Ok(Some(fs::read(&dest)?))
        } else {
            Ok(None)
        }
    }

    /// Lista todas as chaves de declaração no store.
    pub fn list_keys(&self) -> std::io::Result<Vec<String>> {
        let mut keys = Vec::new();
        let surgical_root = self.store_dir.join("surgical");
        if surgical_root.is_dir() {
            for prefix_entry in fs::read_dir(&surgical_root)? {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_declarations_basic() {
        let source = "def baseNat : Nat := 100\n\ntheorem baseId {α : Type} (x : α) : α := x\n\nlemma baseAdd (n m : Nat) : Nat := n + m\n";
        let decls = extract_declarations(source);
        assert_eq!(decls.len(), 3);
        assert_eq!(decls[0].name, "baseNat");
        assert_eq!(decls[0].kind, DeclKind::Def);
        assert_eq!(decls[1].name, "baseId");
        assert_eq!(decls[1].kind, DeclKind::Theorem);
        assert_eq!(decls[2].name, "baseAdd");
        assert_eq!(decls[2].kind, DeclKind::Lemma);
    }

    #[test]
    fn decl_key_is_deterministic() {
        let k1 = surgical_decl_key("def x : Nat := 1", &[], &None);
        let k2 = surgical_decl_key("def x : Nat := 1", &[], &None);
        assert_eq!(k1, k2);
        let k3 = surgical_decl_key("def x : Nat := 2", &[], &None);
        assert_ne!(k1, k3);
    }

    #[test]
    fn surgical_store_roundtrip() -> std::io::Result<()> {
        let dir = tempfile_dir()?;
        let store = SurgicalStore::open(&dir)?;
        let key = "abcd1234";
        let content = b"def x : Nat := 1\n";
        store.store_decl(key, content)?;
        assert!(store.contains(key));
        let restored = store.restore_decl(key)?;
        assert_eq!(restored, Some(content.to_vec()));
        Ok(())
    }

    fn tempfile_dir() -> std::io::Result<PathBuf> {
        let dir = std::env::temp_dir().join(format!("lean-cache-surgical-{}", std::process::id()));
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}
