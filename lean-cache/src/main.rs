#![allow(dead_code)]

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

mod bench;
mod compression;
mod remote;
mod s3;
mod sha256;
mod snapshots;
mod store;
mod surgical;
mod worker;

use bench::executar_suite;
use remote::{push_key, sync_all, RemoteClient, RemoteConfig};
use s3::{pull_key_from_s3, push_key_to_s3, S3Client, S3Config};
use snapshots::{snapshot_key, ElaborationSnapshot, LeanServer, SnapshotStore};
use store::{module_key, resolve_import_graph, ContentStore};
use surgical::{extract_declarations, surgical_decl_key, SurgicalStore};
use worker::{BuildRequest, RemoteWorkerClient, WorkerConfig, WorkerPool};

const ARTIFACT_EXTENSIONS: &[&str] = &["olean", "ilean", "c", "o"];

fn now_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Encontra o executável `lean` no PATH.
fn find_lake() -> Option<PathBuf> {
    // Tenta caminhos comuns do elan primeiro (caminho absoluto).
    if let Ok(home) = env::var("HOME") {
        let candidates = [
            format!("{}/.elan/bin/lake", home),
            format!("{}/.local/share/elan/bin/lake", home),
        ];
        for c in &candidates {
            if Path::new(c).is_file() {
                return Some(PathBuf::from(c));
            }
        }
    }
    // Tenta `lake` direto no PATH.
    if let Ok(path) = env::var("PATH") {
        for dir in path.split(':') {
            let p = Path::new(dir).join("lake");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

#[allow(dead_code)]
fn which_lean(name: &str) -> Option<PathBuf> {
    if let Ok(path) = env::var("PATH") {
        for dir in path.split(':') {
            let p = Path::new(dir).join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Imprime usage.
fn print_usage() {
    println!(
        "Uso: lean-cache <comando> [argumentos]\n\
         Comandos:\n\
          init                      Inicializa o store de cache (.lean-cache)\n\
          hash <arquivo.lean>       Calcula a chave de conteúdo de um módulo\n\
          store <chave> <dir>       Armazena artefatos de um diretório sob uma chave\n\
          get   <chave> <dir>       Restaura artefatos de uma chave para um diretório\n\
          build <arquivo.raiz.lean> Build acelerado: usa cache, senao executa lean\n\
          status                    Mostra estatísticas do store\n\
          clean                     Remove o store de cache local\n\
          gc                        Garbage collection LRU (remove entradas menos acessadas)\n\
          remote-set <url>          Define URL base do cache remoto\n\
          push <chave>              Envia artefatos de uma chave para o remoto\n\
          pull <chave>              Baixa artefatos de uma chave do remoto\n\
          sync                      Sincroniza chaves locais para o remoto\n\
          s3-push <chave>           Envia artefatos de uma chave para o S3\n\
          s3-pull <chave>           Baixa artefatos de uma chave do S3\n\
          s3-sync                   Sincroniza chaves locais para o S3\n\
          snap <chave>              Salva snapshot de elaboração de um módulo\n\
          snap-load <chave>         Restaura snapshot de elaboração de um módulo\n\
          snap-list                 Lista snapshots armazenados\n\
          surgical-list <arquivo.lean>  Lista declaracoes cirurgicas de um arquivo\n\
          surgical-store <arquivo.lean> Armazena declaracoes cirurgicas no cache\n\
          surgical-get <arquivo.lean>   Restaura declaracoes cirurgicas do cache\n\
          bench                     Executa suite de benchmark (simulado)

Variavel de ambiente:
  LEAN_CACHE_DIR   Diretorio do store de cache (padrao: .lean-cache)"
    );
}

/// Comando init.
fn cmd_init(store_dir: &Path) -> io::Result<()> {
    fs::create_dir_all(store_dir)?;
    fs::create_dir_all(store_dir.join("objects"))?;
    let config_path = store_dir.join("config.txt");
    let config = format!(
        "store_dir={}\ncreated_at_ns={}\n",
        store_dir.display(),
        now_ns()
    );
    fs::write(&config_path, config)?;
    println!("Store inicializado em: {}", store_dir.display());
    println!(
        "Artefatos serao guardados em: {}",
        store_dir.join("objects").display()
    );
    Ok(())
}

/// Comando hash.
fn cmd_hash(file: &Path) -> io::Result<()> {
    if !file.is_file() {
        eprintln!("Erro: arquivo nao encontrado: {}", file.display());
        std::process::exit(1);
    }
    let data = fs::read(file)?;
    let key = sha256::sha256_hex(&data);
    println!("{}", key);
    Ok(())
}

/// Comando store.
fn cmd_store(key: &str, dir: &Path) -> io::Result<()> {
    if !dir.is_dir() {
        eprintln!("Erro: diretorio nao encontrado: {}", dir.display());
        std::process::exit(1);
    }
    let store = ContentStore::open(store_dir())?;
    let mut artifacts: Vec<PathBuf> = Vec::new();
    for ext in ARTIFACT_EXTENSIONS {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                if let Some(ext_name) = path.extension() {
                    if ext_name == std::ffi::OsStr::new(ext) {
                        artifacts.push(path);
                    }
                }
            }
        }
    }
    store.store(key, &artifacts)?;
    println!(
        "Armazenados {} artefato(s) sob a chave {}",
        artifacts.len(),
        key
    );
    Ok(())
}

/// Comando get.
fn cmd_get(key: &str, dest: &Path) -> io::Result<()> {
    let store = ContentStore::open(store_dir())?;
    let restored = store.restore(key, dest)?;
    println!(
        "Restaurados {} artefato(s) da chave {} para {}",
        restored.len(),
        key,
        dest.display()
    );
    Ok(())
}

/// Comando status.
fn cmd_status() -> io::Result<()> {
    let store = ContentStore::open(store_dir())?;
    let keys = store.list_keys()?;
    let size = store.size_bytes()?;
    println!("Status do store (.lean-cache)");
    println!("  Chaves: {}", keys.len());
    println!("  Tamanho aproximado: {} bytes ({} KB)", size, size / 1024);
    println!("  Compressao: gzip (acima de 64 KB)");
    println!("  Hard-link na restauracao: ativado");
    println!("  Quota maxima: sem limite (0 bytes)");
    for k in keys.iter().take(10) {
        println!("    - {}", k);
    }
    if keys.len() > 10 {
        println!("    ... e mais {}", keys.len() - 10);
    }
    Ok(())
}

/// Comando remote-set: define a URL base do cache remoto.
fn cmd_remote_set(url: &str) -> io::Result<()> {
    let store_dir = store_dir();
    fs::create_dir_all(&store_dir)?;
    let mut config = RemoteConfig::load(&store_dir);
    config.base_url = url.to_string();
    config.save(&store_dir)?;
    println!("Cache remoto configurado: {}", url);
    Ok(())
}

/// Comando push: envia os artefatos de uma chave para o remoto.
fn cmd_push(key: &str) -> io::Result<()> {
    let store_dir = store_dir();
    let store = ContentStore::open(&store_dir)?;
    let config = RemoteConfig::load(&store_dir);
    if config.base_url.is_empty() {
        eprintln!("Erro: nenhum cache remoto configurado. Use 'lean-cache remote-set <url>'.");
        std::process::exit(1);
    }
    let remote = RemoteClient::new(config);
    let (enviados, falharam) = push_key(&store, &remote, key)?;
    println!(
        "Push da chave {}: {} enviado(s), {} falharam.",
        key, enviados, falharam
    );
    Ok(())
}

/// Comando pull: baixa os artefatos de uma chave do remoto para o store local.
/// Usa verificação de integridade (SHA-256) e, se o remoto estiver indisponível,
/// mantém o que já estiver em cache local (fallback implícito).
fn cmd_pull(key: &str) -> io::Result<()> {
    let store_dir = store_dir();
    let store = ContentStore::open(&store_dir)?;
    let config = RemoteConfig::load(&store_dir);
    if config.base_url.is_empty() {
        eprintln!("Erro: nenhum cache remoto configurado. Use 'lean-cache remote-set <url>'.");
        std::process::exit(1);
    }
    let remote = RemoteClient::new(config);

    // Se ja existe localmente, nao precisa baixar (fallback local).
    if store.contains(key) {
        println!(
            "Chave {} ja existe localmente (cache local). Nada a fazer.",
            key
        );
        return Ok(());
    }

    // Tenta verificar se existe no remoto. Se o remoto estiver offline, reporta
    // mas não quebra: o caller (build) pode continuar com fallback para build local.
    let existe_remoto = match remote.key_exists_remote(key) {
        Ok(existe) => existe,
        Err(e) => {
            eprintln!(
                "Aviso: não foi possível consultar o cache remoto ({}), fallback para build local.",
                e
            );
            eprintln!(
                "Chave {} não encontrada localmente e remoto indisponível.",
                key
            );
            return Ok(());
        }
    };
    if !existe_remoto {
        eprintln!(
            "Erro: chave {} não encontrada no remoto nem localmente.",
            key
        );
        std::process::exit(1);
    }

    // Como não listamos remotamente, tentamos baixar com nomes genéricos
    // para cada extensão conhecida. O servidor deve resolver o artefato correto.
    // A chave de conteúdo serve como hash esperado para verificação de integridade.
    let mut baixados = 0usize;
    let mut falharam = 0usize;
    for ext in ARTIFACT_EXTENSIONS {
        let file_name = format!("{}.{}", key, ext);
        let dest = store.object_dir_for(key)?.join(&file_name);
        match remote.download_artifact_verified(key, &file_name, &dest, key) {
            Ok(true) => baixados += 1,
            Ok(false) => falharam += 1, // arquivo não existe ou integridade falhou
            Err(e) => {
                eprintln!("Aviso: erro de I/O ao baixar {}: {}", file_name, e);
                falharam += 1;
            }
        }
    }

    // Remove o diretório se nada baixou.
    let dir = store.object_dir_for(key)?;
    let tem_arquivos =
        dir.is_dir() && fs::read_dir(&dir)?.any(|e| e.map(|e| e.path().is_file()).unwrap_or(false));
    if !tem_arquivos {
        let _ = fs::remove_dir_all(&dir);
        eprintln!(
            "Aviso: nenhum artefato baixado para a chave {} (verifique integridade do remoto).",
            key
        );
    }

    println!(
        "Pull da chave {}: {} baixado(s), {} falharam.",
        key, baixados, falharam
    );
    Ok(())
}

/// Comando sync: faz push de todas as chaves locais ainda nao no remoto.
fn cmd_sync() -> io::Result<()> {
    let store_dir = store_dir();
    let store = ContentStore::open(&store_dir)?;
    let config = RemoteConfig::load(&store_dir);
    if config.base_url.is_empty() {
        eprintln!("Erro: nenhum cache remoto configurado. Use 'lean-cache remote-set <url>'.");
        std::process::exit(1);
    }
    let remote = RemoteClient::new(config);
    let (ok, fail, skipped) = sync_all(&store, &remote)?;
    println!(
        "Sincronizacao: {} enviado(s), {} falharam, {} ja no remoto.",
        ok, fail, skipped
    );
    Ok(())
}

/// Comando clean.
fn cmd_clean() -> io::Result<()> {
    let store_dir = store_dir();
    if store_dir.exists() {
        fs::remove_dir_all(&store_dir)?;
        println!("Store removido: {}", store_dir.display());
    } else {
        println!("Nenhum store encontrado em: {}", store_dir.display());
    }
    Ok(())
}

/// Comando gc: garante que o store nao exceda a quota configurada (max_store_bytes),
/// removendo entradas menos recentemente acessadas (LRU).
fn cmd_gc() -> io::Result<()> {
    let store_dir = store_dir();
    let store = ContentStore::open(&store_dir)?;
    let (removidas, liberados) = store.cache_gc()?;
    println!(
        "GC: {} chave(s) removida(s), {} bytes liberados.",
        removidas, liberados
    );
    Ok(())
}

/// Comando surgical-list: lista declaracoes cirurgicas de um arquivo Lean.
fn cmd_surgical_list(source_path: &Path) -> io::Result<()> {
    if !source_path.is_file() {
        eprintln!("Erro: arquivo nao encontrado: {}", source_path.display());
        std::process::exit(1);
    }
    let source = fs::read_to_string(source_path)?;
    let declarations = extract_declarations(&source);

    println!("Declaracoes cirurgicas em {}:", source_path.display());
    for decl in &declarations {
        let key_short = &decl.content_hash[..8];
        println!(
            "  {} {} (linha {}-{}, chave {})",
            decl.kind.kind_as_str(),
            decl.name,
            decl.start_line,
            decl.end_line,
            key_short
        );
    }
    println!("Total: {} declaracao(oes).", declarations.len());
    Ok(())
}

/// Comando surgical-store: armazena declaracoes cirurgicas de um arquivo no cache.
fn cmd_surgical_store(source_path: &Path) -> io::Result<()> {
    if !source_path.is_file() {
        eprintln!("Erro: arquivo nao encontrado: {}", source_path.display());
        std::process::exit(1);
    }
    let source = fs::read_to_string(source_path)?;
    let declarations = extract_declarations(&source);
    let surgical_store = SurgicalStore::open(store_dir())?;
    let lean_version = None; // Sem versao do Lean para uso standalone
    let _flags: Vec<String> = Vec::new();

    let mut stored = 0usize;
    let mut skipped = 0usize;

    // Precisamos das chaves dos modulos importados para calcular dep_keys.
    // Para uso standalone, usamos apenas o hash do conteudo da declaracao.
    for decl in &declarations {
        let dep_keys: Vec<String> = decl
            .depends_on
            .iter()
            .filter_map(|_| None) // Sem resolucao de modulos em modo standalone
            .collect();
        let decl_key = surgical_decl_key(&decl.content, &dep_keys, &lean_version);

        if surgical_store.contains(&decl_key) {
            skipped += 1;
        } else {
            surgical_store.store_decl(&decl_key, decl.content.as_bytes())?;
            stored += 1;
        }
    }

    println!(
        "Surgical store: {} armazenado(s), {} ja existiam, {} total.",
        stored,
        skipped,
        declarations.len()
    );
    Ok(())
}

/// Comando surgical-get: restaura declaracoes cirurgicas de um arquivo do cache.
fn cmd_surgical_get(source_path: &Path, dest_dir: &Path) -> io::Result<()> {
    if !source_path.is_file() {
        eprintln!("Erro: arquivo nao encontrado: {}", source_path.display());
        std::process::exit(1);
    }
    let source = fs::read_to_string(source_path)?;
    let declarations = extract_declarations(&source);
    let surgical_store = SurgicalStore::open(store_dir())?;
    let lean_version = None;

    fs::create_dir_all(dest_dir)?;

    let mut restored = 0usize;
    let mut missing = 0usize;

    for decl in &declarations {
        let dep_keys: Vec<String> = decl.depends_on.iter().filter_map(|_| None).collect();
        let decl_key = surgical_decl_key(&decl.content, &dep_keys, &lean_version);

        match surgical_store.restore_decl(&decl_key)? {
            Some(content) => {
                let dest = dest_dir.join(format!("{}.lean", decl.name));
                fs::write(&dest, &content)?;
                restored += 1;
            }
            None => {
                missing += 1;
                println!(
                    "  Declaracao nao encontrada no cache: {} (chave {})",
                    decl.name,
                    &decl.content_hash[..8]
                );
            }
        }
    }

    println!(
        "Surgical get: {} restaurado(s), {} nao encontrado(s), {} total.",
        restored,
        missing,
        declarations.len()
    );
    Ok(())
}

/// Comando build: wrapper acelerado ao `lake build`.
///
/// Resolve o grafo de imports, calcula chave de conteúdo por módulo.
/// Se TODOS os módulos estão em cache hit: pulamos o `lake build` inteiro
/// e restauramos os `.olean` diretamente em `.lake/build/lib/lean/<pacote>/`.
/// Se ALGUM módulo está em miss: executamos o `lake build` normalmente e
/// armazenamos os artefatos no cache para a próxima vez.
fn cmd_build(root: &Path) -> io::Result<()> {
    if !root.is_file() {
        eprintln!("Erro: arquivo raiz nao encontrado: {}", root.display());
        std::process::exit(1);
    }

    let lake = match find_lake() {
        Some(l) => l,
        None => {
            eprintln!("Erro: executavel 'lake' nao encontrado no PATH.");
            eprintln!("Instale o Lean 4 (elan) que inclui 'lake'.");
            std::process::exit(1);
        }
    };
    let lake = lake.canonicalize().unwrap_or(lake);
    println!("Usando Lake: {}", lake.display());

    let root_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let roots = vec![root_dir.clone()];

    // Verifica se e um projeto Lake (tem lakefile.toml ou lakefile.lean).
    let is_lake_project =
        root_dir.join("lakefile.toml").is_file() || root_dir.join("lakefile.lean").is_file();
    if !is_lake_project {
        eprintln!(
            "Aviso: nenhum lakefile encontrado. Use 'lean-cache build' dentro de um projeto Lake."
        );
    }

    // Resolve grafo de imports (ordem topologica).
    let graph = match resolve_import_graph(root, &roots) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Erro ao resolver grafo de imports: {}", e);
            std::process::exit(1);
        }
    };

    if graph.is_empty() {
        eprintln!("Nenhum modulo encontrado no grafo de imports.");
        std::process::exit(1);
    }

    let store = ContentStore::open(store_dir())?;
    let surgical_store = SurgicalStore::open(store_dir())?;
    let lean_version = detect_lean_version(&lake)?;
    let _flags: Vec<String> = Vec::new();

    let mut hits = 0usize;
    let mut misses = 0usize;
    let mut all_hit = true;
    let mut module_decisions: Vec<(&str, &Path, &str, String)> = Vec::new();
    // Mapa nome_modulo -> chave calculada. O grafo vem em ordem topologica
    // (dependencias antes do dependente), entao ja podemos resolver chaves
    // dos imports transitivos enquanto iteramos.
    let mut module_keys: HashMap<String, String> = HashMap::new();

    // Contadores de edicao cirurgica (por declaracao).
    let mut decl_hits = 0usize;
    let mut decl_misses = 0usize;

    println!("Grafo de resolucao: {} modulo(s)", graph.len());

    // Primeira passagem: calcular chaves e verificar hit/miss.
    for (module_name, source_path, imports) in &graph {
        let source_bytes = match fs::read(source_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Erro ao ler {}: {}", source_path.display(), e);
                std::process::exit(1);
            }
        };

        // Resolve as chaves dos modulos importados (já calculados pela ordem topologica).
        let import_keys: Vec<String> = imports
            .iter()
            .filter_map(|imp| module_keys.get(imp).cloned())
            .collect();

        // A chave inclui conteudo direto + chaves dos imports + versao do Lean + flags.
        let key = module_key(&source_bytes, &import_keys, &lean_version, &[]);
        module_keys.insert(module_name.to_string(), key.clone());

        // --- Edicao cirurgica: extrair declaracoes individuais do modulo ---
        let source_text = match fs::read_to_string(source_path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!(
                    "Erro ao ler {} para analise cirurgica: {}",
                    source_path.display(),
                    e
                );
                std::process::exit(1);
            }
        };
        let declarations = extract_declarations(&source_text);

        for decl in &declarations {
            // Chaves das declaracoes de que esta declaracao depende (ja calculadas pela ordem topologica).
            let dep_keys: Vec<String> = decl
                .depends_on
                .iter()
                .filter_map(|dep_name| module_keys.get(dep_name).cloned())
                .collect();
            let decl_key = surgical_decl_key(&decl.content, &dep_keys, &lean_version);

            if surgical_store.contains(&decl_key) {
                decl_hits += 1;
            } else {
                decl_misses += 1;
                // Armazena a declaracao cirurgica para reuso futuro.
                let _ = surgical_store.store_decl(&decl_key, decl.content.as_bytes());
            }
        }

        if store.contains(&key) {
            hits += 1;
            println!("[HIT]   {} (chave {})", module_name, &key[..8]);
            module_decisions.push(("HIT", source_path, module_name, key));
        } else {
            misses += 1;
            all_hit = false;
            println!("[MISS]  {} (chave {})", module_name, &key[..8]);
            module_decisions.push(("MISS", source_path, module_name, key));
        }
    }

    if all_hit {
        println!("\nTodos os modulos estao em cache. Pulando lake build...");
        // Restaura artefatos diretamente no .lake/build/lib/lean/<pacote>/.
        let lake_build_dir = root_dir.join(".lake/build/lib/lean");
        for (status, _source_path, module_name, key) in &module_decisions {
            if *status != "HIT" {
                continue;
            }
            let mod_out = root_dir.join(format!(".lean-cache-out/{}", module_name));
            let restored = store.restore(key, &mod_out)?;
            if !restored.is_empty() {
                fs::create_dir_all(&lake_build_dir)?;
                restore_artifacts_to_lake(&mod_out, &lake_build_dir, module_name)?;
            }
        }
        println!(
            "\nResultado: {} hit(s), 0 miss(es) - build pulado via cache.",
            hits
        );
        println!(
            "  Cirurgico: {} declaracao(oes) hit, {} miss de {} total.",
            decl_hits,
            decl_misses,
            decl_hits + decl_misses
        );
        return Ok(());
    }

    // Antes de executar lake build, tenta buscar no cache remoto os modulos em MISS.
    // Se o remoto tiver a chave, baixamos e tratamos como hit (fallback: se remoto
    // estiver offline, seguimos para lake build local).
    let config = RemoteConfig::load(&store_dir());
    if !config.base_url.is_empty() {
        let remote = RemoteClient::new(config);
        for (status, _source_path, module_name, key) in &module_decisions {
            if *status != "MISS" {
                continue;
            }
            println!(
                "[REMOTO] tentando pull de {} (chave {}) ...",
                module_name,
                &key[..8]
            );
            let store_dir = store_dir();
            let store_tmp = ContentStore::open(&store_dir)?;
            let mut baixou_algo = false;
            for ext in ARTIFACT_EXTENSIONS {
                let file_name = format!("{}.{}", key, ext);
                let dest = store_tmp.object_dir_for(key)?.join(&file_name);
                match remote.download_artifact_verified(key, &file_name, &dest, key) {
                    Ok(true) => {
                        baixou_algo = true;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        eprintln!(
                            "Aviso: erro de I/O ao baixar {} do remoto: {}",
                            file_name, e
                        );
                    }
                }
            }
            if baixou_algo {
                // Marca como HIT via remoto para não executar lake build neste modulo.
                println!("[HIT-REMOTO] {} restaurado via cache remoto.", module_name);
            } else {
                eprintln!(
                    "Aviso: chave {} não encontrada no remoto; será compilado localmente.",
                    key
                );
            }
        }
        println!(
            "\nApós consulta ao remoto: {} hit(s) locais, remoto consultado para {} miss(es).",
            hits, misses
        );
    }

    // Verifica se todos os modulos agora estam resolvidos (hit local ou hit remoto).
    // Como não rastreamos hit-remoto separadamente, executamos lake build se houver
    // qualquer modulo que não tenha artefatos locais após a tentativa de remoto.
    let todos_resolvidos = module_decisions
        .iter()
        .all(|(status, _, _, key)| *status == "HIT" || store.contains(key));

    if todos_resolvidos {
        println!("\nTodos os modulos resolvidos (cache local + remoto). Pulando lake build...");
        let lake_build_dir = root_dir.join(".lake/build/lib/lean");
        for (status, _source_path, module_name, key) in &module_decisions {
            if *status != "HIT" && !store.contains(key) {
                continue;
            }
            let mod_out = root_dir.join(format!(".lean-cache-out/{}", module_name));
            let restored = store.restore(key, &mod_out)?;
            if !restored.is_empty() {
                fs::create_dir_all(&lake_build_dir)?;
                restore_artifacts_to_lake(&mod_out, &lake_build_dir, module_name)?;
            }
        }
        println!(
            "\nResultado: {} hit(s) (local + remoto), build pulado via cache.",
            hits
        );
        println!(
            "  Cirurgico: {} declaracao(oes) hit, {} miss de {} total.",
            decl_hits,
            decl_misses,
            decl_hits + decl_misses
        );
        return Ok(());
    }

    // Pelo menos um modulo ainda esta em MISS: executar o lake build normalmente.
    println!("\nExecutando lake build (modulos ainda em miss após consulta ao remoto)...");

    let output_result = Command::new(&lake)
        .current_dir(&root_dir)
        .arg("build")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    let status = match output_result {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Erro ao executar lake: {}", e);
            std::process::exit(1);
        }
    };

    if !status.success() {
        eprintln!("Erro: falha ao executar lake build (codigo de saida nao zero).");
        std::process::exit(1);
    }

    // Armazena artefatos gerados pelo Lake no cache.
    // Percorre recursivamente .lake/build/lib/lean/ para encontrar TODOS os
    // artefatos (.olean, .ilean, .c, .o) e mapeia cada um para seu modulo.
    let lake_build_dir = root_dir.join(".lake/build/lib/lean");
    if lake_build_dir.is_dir() {
        // Coleta todos os arquivos de artefato no diretorio de build do Lake.
        let mut lake_artifacts: Vec<PathBuf> = Vec::new();
        collect_lake_artifacts(&lake_build_dir, &mut lake_artifacts)?;

        // Para cada modulo em MISS, encontra seus artefatos e armazena no cache.
        for (status, source_path, module_name, key) in &module_decisions {
            if *status != "MISS" {
                continue;
            }

            let file_stem = module_name
                .rsplit('.')
                .next()
                .unwrap_or(module_name)
                .to_string();

            let mut found_artifacts: Vec<PathBuf> = Vec::new();

            // Procura artefatos que correspondem a este modulo.
            // Um artefato pertence ao modulo se o nome do arquivo (sem extensao)
            // bate com o file_stem do source_path.
            for artifact in &lake_artifacts {
                if let Some(stem) = artifact.file_stem().and_then(|s| s.to_str()) {
                    if stem == file_stem {
                        found_artifacts.push(artifact.clone());
                    }
                }
            }

            if !found_artifacts.is_empty() {
                store.store(key, &found_artifacts)?;
                println!(
                    "  Cache: armazenado {} artefato(s) para {} (chave {})",
                    found_artifacts.len(),
                    module_name,
                    &key[..8]
                );
            }

            // Armazena declaracoes cirurgicas do modulo recompilado.
            if let Ok(source_text) = fs::read_to_string(source_path) {
                let declarations = extract_declarations(&source_text);
                for decl in &declarations {
                    let dep_keys: Vec<String> = decl
                        .depends_on
                        .iter()
                        .filter_map(|dep_name| module_keys.get(dep_name).cloned())
                        .collect();
                    let decl_key = surgical_decl_key(&decl.content, &dep_keys, &lean_version);

                    if !surgical_store.contains(&decl_key) {
                        let _ = surgical_store.store_decl(&decl_key, decl.content.as_bytes());
                    }
                }
            }
        }
    }

    println!(
        "\nResultado: {} hit(s), {} miss(es) de {} modulo(s).",
        hits,
        misses,
        graph.len()
    );
    println!(
        "  Cirurgico: {} declaracao(oes) hit, {} miss de {} total.",
        decl_hits,
        decl_misses,
        decl_hits + decl_misses
    );
    Ok(())
}

/// Copia artefatos restaurados do cache para o diretorio de destino.
/// (Mantido para compatibilidade com outros comandos.)
fn copy_artifacts_to_source(src_dir: &Path, dest_dir: &Path) -> io::Result<()> {
    if !src_dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(src_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension() {
                if ARTIFACT_EXTENSIONS.contains(&ext.to_string_lossy().as_ref()) {
                    let file_name = path.file_name().unwrap();
                    let dest = dest_dir.join(file_name);
                    fs::copy(&path, &dest)?;
                }
            }
        }
    }
    Ok(())
}

/// Percorre recursivamente um diretorio coletando todos os arquivos de artefato Lean
/// (.olean, .ilean, .c, .o).
fn collect_lake_artifacts(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_lake_artifacts(&path, out)?;
        } else if path.is_file() {
            if let Some(ext) = path.extension() {
                if ARTIFACT_EXTENSIONS.contains(&ext.to_string_lossy().as_ref()) {
                    out.push(path);
                }
            }
        }
    }
    Ok(())
}

/// Valida um nome de módulo Lean para prevenir path traversal.
///
/// Regras:
/// - Não pode conter `..` (path traversal)
/// - Não pode começar com `/` ou `.`
/// - Não pode conter byte nulo
/// - Cada segmento separado por `.` deve ser um nome válido (alfanumérico, underscore, hífen)
fn validate_module_name(name: &str) -> io::Result<String> {
    if name.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "nome do módulo não pode estar vazio",
        ));
    }
    if name.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "nome do módulo não pode conter byte nulo",
        ));
    }
    if name.starts_with("/") || name.starts_with("./") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "nome do módulo não pode começar com / ou ./",
        ));
    }
    if name.contains("..") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "nome do módulo não pode conter path traversal (..)",
        ));
    }
    // Valida cada segmento separado por ponto
    for segment in name.split('.') {
        if segment.is_empty() || segment == ".." || segment == "." {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("segmento de módulo inválido: '{}'", segment),
            ));
        }
        if !segment
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "segmento de módulo contém caractere inválido: '{}'",
                    segment
                ),
            ));
        }
    }
    Ok(name.to_string())
}

/// Restaura artefatos do cache para o diretorio de build do Lake,
/// preservando a estrutura de pacotes (ex: Testproj/Mod1.olean).
/// O `mod_out` contem arquivos restaurados com nomes como `Mod1.olean`.
/// O `lake_build_dir` e `.lake/build/lib/lean`.
fn restore_artifacts_to_lake(
    mod_out: &Path,
    lake_build_dir: &Path,
    module_name: &str,
) -> io::Result<()> {
    if !mod_out.is_dir() {
        return Ok(());
    }

    // Determina o subdiretorio do pacote baseado no nome do modulo.
    // Ex: "Testproj.Mod1" -> subdir "Testproj", arquivo "Mod1.olean".
    // Ex: "Main" -> subdir "" (raiz), arquivo "Main.olean".
    //
    // Segurança: valida module_name para prevenir path traversal.
    let validated_name = validate_module_name(module_name)?;
    let (pkg_subdir, file_base) = if let Some(dot_pos) = validated_name.rfind('.') {
        let pkg =
            &validated_name[..dot_pos].replace('.', std::path::MAIN_SEPARATOR.to_string().as_str());
        let base = &validated_name[dot_pos + 1..];
        (pkg.to_string(), base.to_string())
    } else {
        (String::new(), validated_name.to_string())
    };

    let target_dir = if pkg_subdir.is_empty() {
        lake_build_dir.to_path_buf()
    } else {
        lake_build_dir.join(&pkg_subdir)
    };
    fs::create_dir_all(&target_dir)?;

    for entry in fs::read_dir(mod_out)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension() {
                if ARTIFACT_EXTENSIONS.contains(&ext.to_string_lossy().as_ref()) {
                    let file_name = format!("{}.{}", file_base, ext.to_string_lossy());
                    let dest = target_dir.join(&file_name);
                    // Tenta hard-link primeiro, depois copia.
                    if std::fs::hard_link(&path, &dest).is_err() {
                        fs::copy(&path, &dest)?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Detecta a versao do Lean instalado.
fn detect_lean_version(lean: &Path) -> io::Result<Option<String>> {
    let output = Command::new(lean).arg("--version").output()?;
    if output.status.success() {
        let txt = String::from_utf8_lossy(&output.stdout);
        let v = txt.trim().to_string();
        Ok(Some(v))
    } else {
        Ok(None)
    }
}

/// Le a configuracao S3 a partir de variaveis de ambiente.
/// Espera: S3_BUCKET, S3_REGION, S3_ACCESS_KEY, S3_SECRET_KEY, S3_ENDPOINT (opcional).
fn s3_config_from_env() -> io::Result<S3Config> {
    let bucket = std::env::var("S3_BUCKET")
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "variavel S3_BUCKET nao definida"))?;
    let region = std::env::var("S3_REGION")
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "variavel S3_REGION nao definida"))?;
    let access_key = std::env::var("S3_ACCESS_KEY").map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "variavel S3_ACCESS_KEY nao definida",
        )
    })?;
    let secret_key = std::env::var("S3_SECRET_KEY").map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "variavel S3_SECRET_KEY nao definida",
        )
    })?;
    let endpoint = std::env::var("S3_ENDPOINT").ok();
    Ok(S3Config {
        bucket,
        region,
        endpoint,
        access_key,
        secret_key,
        no_sign: false,
    })
}

/// Caminho do arquivo de configuracao de workers (.lean-cache/workers.txt).
fn workers_config_path() -> PathBuf {
    PathBuf::from(store_dir().join("workers.txt"))
}

/// Carrega a lista de workers do arquivo de configuracao.
fn load_workers() -> Vec<WorkerConfig> {
    let path = workers_config_path();
    if !path.is_file() {
        return Vec::new();
    }
    let mut workers = Vec::new();
    if let Ok(data) = std::fs::read_to_string(&path) {
        for line in data.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            workers.push(WorkerConfig {
                url: line.to_string(),
                auth_token: None,
                timeout_secs: 30,
                max_retries: 3,
                weight: 1,
            });
        }
    }
    workers
}

/// Salva a lista de workers no arquivo de configuracao.
fn save_workers(workers: &[WorkerConfig]) -> io::Result<()> {
    let path = workers_config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut lines = Vec::new();
    for w in workers {
        lines.push(w.url.clone());
    }
    std::fs::write(path, lines.join("\n") + "\n")
}

/// Retorna o diretorio do store de cache.
/// Usa a variavel de ambiente LEAN_CACHE_DIR se definida, caso contrario padrao ".lean-cache".
fn store_dir() -> PathBuf {
    std::env::var("LEAN_CACHE_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".lean-cache"))
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage();
        return;
    }

    let comando = &args[1];

    match comando.as_str() {
        "init" => {
            let store_dir = store_dir();
            if let Err(e) = cmd_init(&store_dir) {
                eprintln!("Erro no init: {}", e);
                std::process::exit(1);
            }
        }
        "hash" => {
            if args.len() < 3 {
                eprintln!("Uso: lean-cache hash <arquivo.lean>");
                std::process::exit(1);
            }
            if let Err(e) = cmd_hash(Path::new(&args[2])) {
                eprintln!("Erro no hash: {}", e);
                std::process::exit(1);
            }
        }
        "store" => {
            if args.len() < 4 {
                eprintln!("Uso: lean-cache store <chave> <diretorio-com-artefatos>");
                std::process::exit(1);
            }
            if let Err(e) = cmd_store(&args[2], Path::new(&args[3])) {
                eprintln!("Erro no store: {}", e);
                std::process::exit(1);
            }
        }
        "get" => {
            if args.len() < 4 {
                eprintln!("Uso: lean-cache get <chave> <diretorio-destino>");
                std::process::exit(1);
            }
            if let Err(e) = cmd_get(&args[2], Path::new(&args[3])) {
                eprintln!("Erro no get: {}", e);
                std::process::exit(1);
            }
        }
        "build" => {
            if args.len() < 3 {
                eprintln!("Uso: lean-cache build <arquivo.raiz.lean>");
                std::process::exit(1);
            }
            if let Err(e) = cmd_build(Path::new(&args[2])) {
                eprintln!("Erro no build: {}", e);
                std::process::exit(1);
            }
        }
        "status" => {
            if let Err(e) = cmd_status() {
                eprintln!("Erro no status: {}", e);
                std::process::exit(1);
            }
        }
        "clean" => {
            if let Err(e) = cmd_clean() {
                eprintln!("Erro no clean: {}", e);
                std::process::exit(1);
            }
        }
        "gc" => {
            if let Err(e) = cmd_gc() {
                eprintln!("Erro no gc: {}", e);
                std::process::exit(1);
            }
        }
        "remote-set" => {
            if args.len() < 3 {
                eprintln!("Uso: lean-cache remote-set <url>");
                std::process::exit(1);
            }
            if let Err(e) = cmd_remote_set(&args[2]) {
                eprintln!("Erro no remote-set: {}", e);
                std::process::exit(1);
            }
        }
        "push" => {
            if args.len() < 3 {
                eprintln!("Uso: lean-cache push <chave>");
                std::process::exit(1);
            }
            if let Err(e) = cmd_push(&args[2]) {
                eprintln!("Erro no push: {}", e);
                std::process::exit(1);
            }
        }
        "pull" => {
            if args.len() < 3 {
                eprintln!("Uso: lean-cache pull <chave>");
                std::process::exit(1);
            }
            if let Err(e) = cmd_pull(&args[2]) {
                eprintln!("Erro no pull: {}", e);
                std::process::exit(1);
            }
        }
        "sync" => {
            if let Err(e) = cmd_sync() {
                eprintln!("Erro no sync: {}", e);
                std::process::exit(1);
            }
        }
        "bench" => {
            if let Err(e) = executar_suite() {
                eprintln!("Erro no bench: {}", e);
                std::process::exit(1);
            }
        }
        "surgical-list" => {
            if args.len() < 3 {
                eprintln!("Uso: lean-cache surgical-list <arquivo.lean>");
                std::process::exit(1);
            }
            if let Err(e) = cmd_surgical_list(Path::new(&args[2])) {
                eprintln!("Erro no surgical-list: {}", e);
                std::process::exit(1);
            }
        }
        "surgical-store" => {
            if args.len() < 3 {
                eprintln!("Uso: lean-cache surgical-store <arquivo.lean>");
                std::process::exit(1);
            }
            if let Err(e) = cmd_surgical_store(Path::new(&args[2])) {
                eprintln!("Erro no surgical-store: {}", e);
                std::process::exit(1);
            }
        }
        "surgical-get" => {
            if args.len() < 4 {
                eprintln!("Uso: lean-cache surgical-get <arquivo.lean> <diretorio-destino>");
                std::process::exit(1);
            }
            if let Err(e) = cmd_surgical_get(Path::new(&args[2]), Path::new(&args[3])) {
                eprintln!("Erro no surgical-get: {}", e);
                std::process::exit(1);
            }
        }
        "s3-push" => {
            if args.len() < 3 {
                eprintln!("Uso: lean-cache s3-push <chave>");
                std::process::exit(1);
            }
            let key = &args[2];
            let store_dir = store_dir();
            let store = match ContentStore::open(&store_dir) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Erro ao abrir store: {}", e);
                    std::process::exit(1);
                }
            };
            let config = match s3_config_from_env() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Erro ao ler configuracao S3: {}", e);
                    eprintln!("Defina S3_BUCKET, S3_REGION, S3_ACCESS_KEY, S3_SECRET_KEY (e opcional S3_ENDPOINT).");
                    std::process::exit(1);
                }
            };
            let client = S3Client::new(config);
            match push_key_to_s3(&store, &client, key) {
                Ok((enviados, falharam)) => {
                    println!(
                        "S3 push da chave {}: {} enviado(s), {} falharam.",
                        key, enviados, falharam
                    );
                }
                Err(e) => {
                    eprintln!("Erro no S3 push: {}", e);
                    std::process::exit(1);
                }
            }
        }
        "s3-pull" => {
            if args.len() < 3 {
                eprintln!("Uso: lean-cache s3-pull <chave>");
                std::process::exit(1);
            }
            let key = &args[2];
            let store_dir = store_dir();
            let store = match ContentStore::open(&store_dir) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Erro ao abrir store: {}", e);
                    std::process::exit(1);
                }
            };
            let config = match s3_config_from_env() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Erro ao ler configuracao S3: {}", e);
                    std::process::exit(1);
                }
            };
            let client = S3Client::new(config);
            match pull_key_from_s3(&store, &client, key) {
                Ok((baixados, falharam)) => {
                    println!(
                        "S3 pull da chave {}: {} baixado(s), {} falharam.",
                        key, baixados, falharam
                    );
                }
                Err(e) => {
                    eprintln!("Erro no S3 pull: {}", e);
                    std::process::exit(1);
                }
            }
        }
        "s3-sync" => {
            let store_dir = store_dir();
            let store = match ContentStore::open(&store_dir) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Erro ao abrir store: {}", e);
                    std::process::exit(1);
                }
            };
            let config = match s3_config_from_env() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Erro ao ler configuracao S3: {}", e);
                    std::process::exit(1);
                }
            };
            let client = S3Client::new(config);
            let keys = match store.list_keys() {
                Ok(k) => k,
                Err(e) => {
                    eprintln!("Erro ao listar chaves locais: {}", e);
                    std::process::exit(1);
                }
            };
            let mut enviados = 0usize;
            let mut falharam = 0usize;
            let mut pulados = 0usize;
            for key in &keys {
                // Verifica se pelo menos um artefato ja existe no S3 para esta chave.
                let s3_key = format!("objects/{}/{}.olean", key, key);
                match client.object_exists(&s3_key) {
                    Ok(true) => {
                        pulados += 1;
                        continue;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        eprintln!("Aviso: erro ao verificar S3 para chave {}: {}", key, e);
                        falharam += 1;
                        continue;
                    }
                }
                match push_key_to_s3(&store, &client, key) {
                    Ok((ok, fail)) => {
                        enviados += ok;
                        falharam += fail;
                    }
                    Err(e) => {
                        eprintln!("Erro ao fazer S3 push da chave {}: {}", key, e);
                        falharam += 1;
                    }
                }
            }
            println!(
                "S3 sync: {} enviado(s), {} falharam, {} ja no S3.",
                enviados, falharam, pulados
            );
        }
        "snap" => {
            if args.len() < 3 {
                eprintln!("Uso: lean-cache snap <arquivo.lean|chave>");
                std::process::exit(1);
            }
            let arg = &args[2];
            let store_dir = store_dir();
            let snap_store = match SnapshotStore::open(&store_dir) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Erro ao abrir store de snapshots: {}", e);
                    std::process::exit(1);
                }
            };
            // Detecta versao do Lean, se possivel.
            let lean_version = match find_lake() {
                Some(lake) => match detect_lean_version(&lake) {
                    Ok(v) => v.unwrap_or_else(|| "unknown".to_string()),
                    Err(_) => "unknown".to_string(),
                },
                None => "unknown".to_string(),
            };

            // Se o argumento for um arquivo existente, captura o estado real via lean --server.
            let (module_name, source_hash, state_data) = if Path::new(arg).is_file() {
                let p = Path::new(arg);
                let bytes = match fs::read(p) {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("Erro ao ler {}: {}", p.display(), e);
                        std::process::exit(1);
                    }
                };
                let hash = sha256::sha256_hex(&bytes);
                let mod_name = p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string();

                // Tenta capturar o estado de elaboracao real via lean --server.
                let mut captured = Vec::new();
                if let Ok(mut server) = LeanServer::new() {
                    match server.capture_elaboration_state(p) {
                        Ok(Some(data)) => captured = data,
                        Ok(None) => {
                            eprintln!("Aviso: lean --server nao suportou captura de estado; salvando snapshot vazio.");
                        }
                        Err(e) => {
                            eprintln!("Aviso: falha ao capturar estado via lean --server: {}", e);
                        }
                    }
                    let _ = server.close();
                } else {
                    eprintln!("Aviso: lean --server nao disponivel; salvando snapshot vazio.");
                }

                (mod_name, hash, captured)
            } else {
                (arg.clone(), arg.clone(), Vec::new())
            };

            let key = snapshot_key(&module_name, &source_hash, &lean_version);
            let snapshot = ElaborationSnapshot::new(
                module_name,
                source_hash,
                lean_version,
                state_data,
                Vec::new(),
            );
            match snap_store.save_snapshot(&key, &snapshot) {
                Ok(()) => println!("Snapshot salvo com chave: {}", key),
                Err(e) => {
                    eprintln!("Erro ao salvar snapshot: {}", e);
                    std::process::exit(1);
                }
            }
        }
        "snap-load" => {
            if args.len() < 3 {
                eprintln!("Uso: lean-cache snap-load <chave>");
                std::process::exit(1);
            }
            let key = &args[2];
            let store_dir = store_dir();
            let snap_store = match SnapshotStore::open(&store_dir) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Erro ao abrir store de snapshots: {}", e);
                    std::process::exit(1);
                }
            };
            match snap_store.load_snapshot(key) {
                Ok(Some(snap)) => {
                    println!("Snapshot carregado: chave={}", key);
                    println!("  modulo: {}", snap.module_name);
                    println!("  versao Lean: {}", snap.lean_version);
                    println!("  tamanho: {} bytes", snap.size_bytes);
                    println!("  declaracoes: {}", snap.declaration_keys.len());
                }
                Ok(None) => {
                    println!("Nenhum snapshot encontrado para a chave: {}", key);
                }
                Err(e) => {
                    eprintln!("Erro ao carregar snapshot: {}", e);
                    std::process::exit(1);
                }
            }
        }
        "snap-list" => {
            let store_dir = store_dir();
            let snap_store = match SnapshotStore::open(&store_dir) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Erro ao abrir store de snapshots: {}", e);
                    std::process::exit(1);
                }
            };
            match snap_store.list_keys() {
                Ok(keys) => {
                    println!("Snapshots armazenados: {}", keys.len());
                    for k in keys.iter().take(20) {
                        println!("  - {}", k);
                    }
                    if keys.len() > 20 {
                        println!("  ... e mais {}", keys.len() - 20);
                    }
                }
                Err(e) => {
                    eprintln!("Erro ao listar snapshots: {}", e);
                    std::process::exit(1);
                }
            }
        }
        "worker-add" => {
            if args.len() < 3 {
                eprintln!("Uso: lean-cache worker-add <url>");
                std::process::exit(1);
            }
            let url = &args[2];
            let mut workers = load_workers();
            // Evita duplicados.
            if workers.iter().any(|w| w.url == *url) {
                println!("Worker ja existe: {}", url);
                return;
            }
            workers.push(WorkerConfig {
                url: url.clone(),
                auth_token: None,
                timeout_secs: 30,
                max_retries: 3,
                weight: 1,
            });
            if let Err(e) = save_workers(&workers) {
                eprintln!("Erro ao salvar worker: {}", e);
                std::process::exit(1);
            }
            println!("Worker adicionado: {} (total: {})", url, workers.len());
        }
        "worker-list" => {
            let workers = load_workers();
            println!("Workers configurados: {}", workers.len());
            for (i, w) in workers.iter().enumerate() {
                println!("  {}. {}", i + 1, w.url);
            }
        }
        "worker-build" => {
            if args.len() < 3 {
                eprintln!("Uso: lean-cache worker-build <arquivo.raiz.lean>");
                std::process::exit(1);
            }
            let root = Path::new(&args[2]);
            if !root.is_file() {
                eprintln!("Erro: arquivo raiz nao encontrado: {}", root.display());
                std::process::exit(1);
            }

            let workers = load_workers();
            if workers.is_empty() {
                eprintln!("Erro: nenhum worker configurado. Use 'lean-cache worker-add <url>'.");
                std::process::exit(1);
            }

            let lake = match find_lake() {
                Some(l) => l,
                None => {
                    eprintln!("Erro: executavel 'lake' nao encontrado no PATH.");
                    std::process::exit(1);
                }
            };
            let lake = lake.canonicalize().unwrap_or(lake);
            println!("Usando Lake: {}", lake.display());

            let root_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let roots = vec![root_dir.clone()];

            let is_lake_project = root_dir.join("lakefile.toml").is_file()
                || root_dir.join("lakefile.lean").is_file();
            if !is_lake_project {
                eprintln!("Aviso: nenhum lakefile encontrado. Use 'lean-cache worker-build' dentro de um projeto Lake.");
            }

            let graph = match resolve_import_graph(root, &roots) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("Erro ao resolver grafo de imports: {}", e);
                    std::process::exit(1);
                }
            };

            if graph.is_empty() {
                eprintln!("Nenhum modulo encontrado no grafo de imports.");
                std::process::exit(1);
            }

            let store = match ContentStore::open(store_dir()) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Erro ao abrir store: {}", e);
                    std::process::exit(1);
                }
            };
            let lean_version = match detect_lean_version(&lake) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("Erro ao detectar versao do Lean: {}", e);
                    std::process::exit(1);
                }
            };

            let mut module_keys: HashMap<String, String> = HashMap::new();
            let mut pool = WorkerPool::new();
            for w in &workers {
                pool.add_worker(w.clone());
            }

            println!("Grafo de resolucao: {} modulo(s)", graph.len());
            println!("Workers disponiveis: {}", pool.len());

            for (module_name, source_path, imports) in &graph {
                let source_bytes = match fs::read(source_path) {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("Erro ao ler {}: {}", source_path.display(), e);
                        std::process::exit(1);
                    }
                };

                let import_keys: Vec<String> = imports
                    .iter()
                    .filter_map(|imp| module_keys.get(imp).cloned())
                    .collect();

                let key = module_key(&source_bytes, &import_keys, &lean_version, &[]);
                module_keys.insert(module_name.to_string(), key.clone());

                if store.contains(&key) {
                    println!("[HIT-LOCAL]   {} (chave {})", module_name, &key[..8]);
                    continue;
                }

                // Tenta enviar para um worker remoto (round-robin).
                let mut enviado = false;
                let mut tentativas = 0;
                while tentativas < pool.len() {
                    tentativas += 1;
                    if let Some(worker) = pool.next() {
                        let client = RemoteWorkerClient::new(worker.clone());
                        match client.is_online() {
                            Ok(true) => {}
                            Ok(false) => {
                                eprintln!("Worker offline: {}", worker.url);
                                continue;
                            }
                            Err(e) => {
                                eprintln!("Erro ao verificar worker {}: {}", worker.url, e);
                                continue;
                            }
                        }

                        let source_text = match fs::read_to_string(source_path) {
                            Ok(t) => t,
                            Err(e) => {
                                eprintln!("Erro ao ler {}: {}", source_path.display(), e);
                                std::process::exit(1);
                            }
                        };

                        let req = BuildRequest {
                            key: key.clone(),
                            module_name: module_name.clone(),
                            source_hash: key.clone(),
                            import_keys: import_keys.clone(),
                            lean_version: lean_version.clone(),
                            flags: Vec::new(),
                            source_content: source_text,
                        };

                        match client.request_build(&store, &req) {
                            Ok(true) => {
                                println!(
                                    "[HIT-WORKER] {} via {} (chave {})",
                                    module_name,
                                    worker.url,
                                    &key[..8]
                                );
                                enviado = true;
                                break;
                            }
                            Ok(false) => {
                                eprintln!(
                                    "Worker {} nao conseguiu buildar {}",
                                    worker.url, module_name
                                );
                                continue;
                            }
                            Err(e) => {
                                eprintln!("Erro com worker {}: {}", worker.url, e);
                                continue;
                            }
                        }
                    }
                }

                if !enviado {
                    println!(
                        "[MISS]  {} (chave {}) - nenhum worker disponível",
                        module_name,
                        &key[..8]
                    );
                }
            }

            println!(
                "\nConcluído. Verifique o store local para artefatos armazenados pelos workers."
            );
        }
        _ => {
            eprintln!("Comando desconhecido: {}", comando);
            print_usage();
            std::process::exit(1);
        }
    }
}
