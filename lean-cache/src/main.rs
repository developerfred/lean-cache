use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

mod sha256;
mod store;
mod remote;
mod bench;

use store::{ContentStore, TraceRecord, resolve_import_graph, module_key};
use remote::{RemoteClient, RemoteConfig, push_key, sync_all};
use bench::executar_suite;

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
          remote-set <url>          Define URL base do cache remoto\n\
          push <chave>              Envia artefatos de uma chave para o remoto\n\
          pull <chave>              Baixa artefatos de uma chave do remoto\n\
          sync                      Sincroniza chaves locais para o remoto\n\
          bench                     Executa suite de benchmark (simulado)"
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
    println!("Artefatos serao guardados em: {}", store_dir.join("objects").display());
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
    let store = ContentStore::open(".lean-cache")?;
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
    println!("Armazenados {} artefato(s) sob a chave {}", artifacts.len(), key);
    Ok(())
}

/// Comando get.
fn cmd_get(key: &str, dest: &Path) -> io::Result<()> {
    let store = ContentStore::open(".lean-cache")?;
    let restored = store.restore(key, dest)?;
    println!("Restaurados {} artefato(s) da chave {} para {}", restored.len(), key, dest.display());
    Ok(())
}

/// Comando status.
fn cmd_status() -> io::Result<()> {
    let store = ContentStore::open(".lean-cache")?;
    let keys = store.list_keys()?;
    let size = store.size_bytes()?;
    println!("Status do store (.lean-cache)");
    println!("  Chaves: {}", keys.len());
    println!("  Tamanho aproximado: {} bytes ({} KB)", size, size / 1024);
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
    let store_dir = PathBuf::from(".lean-cache");
    fs::create_dir_all(&store_dir)?;
    let mut config = RemoteConfig::load(&store_dir);
    config.base_url = url.to_string();
    config.save(&store_dir)?;
    println!("Cache remoto configurado: {}", url);
    Ok(())
}

/// Comando push: envia os artefatos de uma chave para o remoto.
fn cmd_push(key: &str) -> io::Result<()> {
    let store_dir = PathBuf::from(".lean-cache");
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
fn cmd_pull(key: &str) -> io::Result<()> {
    let store_dir = PathBuf::from(".lean-cache");
    let store = ContentStore::open(&store_dir)?;
    let config = RemoteConfig::load(&store_dir);
    if config.base_url.is_empty() {
        eprintln!("Erro: nenhum cache remoto configurado. Use 'lean-cache remote-set <url>'.");
        std::process::exit(1);
    }
    let remote = RemoteClient::new(config);

    // Se ja existe localmente, nao precisa baixar.
    if store.contains(key) {
        println!("Chave {} ja existe localmente. Nada a fazer.", key);
        return Ok(());
    }

    // Tenta verificar se existe no remoto.
    if !remote.key_exists_remote(key)? {
        eprintln!("Erro: chave {} nao encontrada no remoto.", key);
        std::process::exit(1);
    }

    // Como nao listamos remotamente, tentamos baixar com nomes genericos
    // para cada extensao conhecida. O servidor deve resolver o artefato correto.
    let mut baixados = 0usize;
    let mut falharam = 0usize;
    for ext in ARTIFACT_EXTENSIONS {
        // Tentamos um nome generico baseados na chave; o servidor decide o que retornar.
        let file_name = format!("{}.{}", key, ext);
        let dest = store.object_dir_for(key).join(&file_name);
        match remote.download_artifact(key, &file_name, &dest) {
            Ok(true) => baixados += 1,
            _ => falharam += 1,
        }
    }

    // Remove o diretorio se nada baixou.
    let dir = store.object_dir_for(key);
    let tem_arquivos = dir.is_dir()
        && fs::read_dir(&dir)?
            .any(|e| e.map(|e| e.path().is_file()).unwrap_or(false));
    if !tem_arquivos {
        let _ = fs::remove_dir_all(&dir);
    }

    println!(
        "Pull da chave {}: {} baixado(s), {} falharam.",
        key, baixados, falharam
    );
    Ok(())
}

/// Comando sync: faz push de todas as chaves locais ainda nao no remoto.
fn cmd_sync() -> io::Result<()> {
    let store_dir = PathBuf::from(".lean-cache");
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
    let store_dir = PathBuf::from(".lean-cache");
    if store_dir.exists() {
        fs::remove_dir_all(&store_dir)?;
        println!("Store removido: {}", store_dir.display());
    } else {
        println!("Nenhum store encontrado em: {}", store_dir.display());
    }
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
    let is_lake_project = root_dir.join("lakefile.toml").is_file()
        || root_dir.join("lakefile.lean").is_file();
    if !is_lake_project {
        eprintln!("Aviso: nenhum lakefile encontrado. Use 'lean-cache build' dentro de um projeto Lake.");
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

    let store = ContentStore::open(".lean-cache")?;
    let lean_version = detect_lean_version(&lake)?;
    let flags: Vec<String> = Vec::new();

    let mut hits = 0usize;
    let mut misses = 0usize;
    let mut all_hit = true;
    let mut module_decisions: Vec<(&str, &Path, &str, String)> = Vec::new();

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

        let import_keys: Vec<String> = imports
            .iter()
            .filter_map(|imp| /* restored_modules não usado aqui; chave depende do conteudo direto */ None)
            .collect();

        // A chave inclui o conteudo direto + versao do Lean + flags.
        // Os imports transitivos sao capturados recursivamente pelo Lake.
        let key = module_key(&source_bytes, &import_keys, &lean_version, &flags);

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
        for (status, source_path, module_name, key) in &module_decisions {
            if *status != "HIT" {
                continue;
            }
            let mod_out = root_dir.join(format!(".lean-cache-out/{}", module_name));
            let restored = store.restore(key, &mod_out)?;
            if !restored.is_empty() {
                // Copia artefatos para o diretorio de build do Lake.
                fs::create_dir_all(&lake_build_dir)?;
                copy_artifacts_to_source(&mod_out, &lake_build_dir)?;
            }
        }
        println!("\nResultado: {} hit(s), 0 miss(es) - build pulado via cache.", hits);
        return Ok(());
    }

    // Pelo menos um modulo esta em MISS: executar o lake build normalmente.
    println!("\nExecutando lake build (modulos em miss detectados)...");

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
    // O Lake coloca .olean em .lake/build/lib/lean/<pacote>/<modulo>.olean
    // ou diretamente em .lake/build/lib/lean/<modulo>.olean para modulos na raiz.
    // Para cada modulo em MISS, procuramos seus artefatos e armazenamos com a chave correta.
    let lake_build_dir = root_dir.join(".lake/build/lib/lean");
    if lake_build_dir.is_dir() {
        for (status, source_path, module_name, key) in &module_decisions {
            if *status != "MISS" {
                continue;
            }

            let file_stem = source_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("output");

            let mut found_artifacts: Vec<PathBuf> = Vec::new();

            // Procurar em subdiretorios (pacotes) do lake_build_dir
            if let Ok(entries) = fs::read_dir(&lake_build_dir) {
                for entry in entries.flatten() {
                    let pkg_dir = entry.path();
                    if pkg_dir.is_dir() {
                        for ext in &["olean", "ilean", "c"] {
                            let artifact_path = pkg_dir.join(format!("{}.{}", file_stem, ext));
                            if artifact_path.is_file() {
                                found_artifacts.push(artifact_path);
                            }
                        }
                    }
                }
            }

            // Tambem procurar diretamente na raiz do lake_build_dir (modulos na raiz como Main)
            for ext in &["olean", "ilean", "c"] {
                let artifact_path = lake_build_dir.join(format!("{}.{}", file_stem, ext));
                if artifact_path.is_file() {
                    found_artifacts.push(artifact_path);
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
        }
    }

    println!(
        "\nResultado: {} hit(s), {} miss(es) de {} modulo(s).",
        hits, misses, graph.len()
    );
    Ok(())
}

/// Copia artefatos restaurados do cache para o diretorio de destino.
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

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage();
        return;
    }

    let comando = &args[1];

    match comando.as_str() {
        "init" => {
            let store_dir = PathBuf::from(".lean-cache");
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
        _ => {
            eprintln!("Comando desconhecido: {}", comando);
            print_usage();
            std::process::exit(1);
        }
    }
}
