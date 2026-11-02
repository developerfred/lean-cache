#![allow(dead_code)]

use crate::store::{ContentStore, module_key};
use crate::sha256::sha256_hex;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Resultado de um cenário de benchmark.
pub struct BenchResult {
    pub nome: String,
    pub duracao: Duration,
    pub hits: usize,
    pub misses: usize,
    pub arquivos_processados: usize,
    pub tamanho_store_bytes: u64,
}

/// Suite de benchmark do `lean-cache`.
/// Como não temos Lean instalado, simulamos:
///  - "build frio": gera artefatos falsos (cópia de dados) e armazena no cache.
///  - "build quente": restaura artefatos do cache (hit).
pub struct BenchSuite {
    pub tmp_dir: PathBuf,
    pub store: ContentStore,
}

impl BenchSuite {
    pub fn novo() -> std::io::Result<Self> {
        let tmp_dir = std::env::temp_dir().join(format!(
            "lean-cache-bench-{}",
            std::process::id()
        ));
        fs::create_dir_all(&tmp_dir)?;
        let store = ContentStore::open(tmp_dir.join(".lean-cache"))?;
        Ok(Self { tmp_dir, store })
    }

    /// Gera um arquivo fonte falso de tamanho aproximado `tamanho_bytes`.
    fn gerar_fonte_falsa(&self, nome: &str, tamanho_bytes: usize) -> std::io::Result<PathBuf> {
        let caminho = self.tmp_dir.join(nome);
        let dados: Vec<u8> = (0..tamanho_bytes).map(|i| (i % 256) as u8).collect();
        fs::write(&caminho, &dados)?;
        Ok(caminho)
    }

    /// Gera artefatos falsos (simulando .olean, .ilean, etc.) ao lado de um fonte.
    fn gerar_artefatos_falsos(&self, fonte: &Path) -> std::io::Result<Vec<PathBuf>> {
        let mut artefatos = Vec::new();
        let dir = fonte.parent().unwrap_or(Path::new("."));
        for ext in &["olean", "ilean", "c", "o"] {
            let caminho = dir.join(format!("artefato_falso.{}", ext));
            let dados = vec![0u8; 64 * 1024]; // 64 KB por artefato
            fs::write(&caminho, &dados)?;
            artefatos.push(caminho);
        }
        Ok(artefatos)
    }

    /// Cenário 1: build frio (miss em tudo) — gera artefatos e armazena.
    pub fn build_frio(&mut self, n_modulos: usize, tamanho_fonte: usize) -> std::io::Result<BenchResult> {
        let inicio = Instant::now();
        let mut misses = 0usize;
        let mut arquivos = 0usize;

        for i in 0..n_modulos {
            let fonte = self.gerar_fonte_falsa(&format!("mod_{}.lean", i), tamanho_fonte)?;
            let dados = fs::read(&fonte)?;
            let chave = module_key(&dados, &[], &None, &[]);

            // Simula build: gera artefatos.
            let artefatos = self.gerar_artefatos_falsos(&fonte)?;
            arquivos += artefatos.len();

            // Armazena no cache (miss).
            self.store.store(&chave, &artefatos)?;
            self.store.save_trace(
                &chave,
                &crate::store::TraceRecord {
                    module: format!("Mod{}", i),
                    source_file: fonte.to_string_lossy().to_string(),
                    key: chave.clone(),
                    lean_version: None,
                    flags: Vec::new(),
                    imports: Vec::new(),
                    artifact_extensions: vec!["olean".to_string(), "ilean".to_string(), "c".to_string(), "o".to_string()],
                    built_at_ns: None,
                    last_accessed_ns: None,
                },
            )?;
            misses += 1;
        }

        let duracao = inicio.elapsed();
        let tamanho = self.store.size_bytes().unwrap_or(0);

        Ok(BenchResult {
            nome: format!("build frio ({} módulos)", n_modulos),
            duracao,
            hits: 0,
            misses,
            arquivos_processados: arquivos,
            tamanho_store_bytes: tamanho,
        })
    }

    /// Cenário 2: build quente (hit em tudo) — restaura artefatos do cache.
    pub fn build_quente(&mut self, n_modulos: usize, tamanho_fonte: usize) -> std::io::Result<BenchResult> {
        let inicio = Instant::now();
        let mut hits = 0usize;
        let mut arquivos = 0usize;

        for i in 0..n_modulos {
            let fonte = self.tmp_dir.join(format!("mod_{}.lean", i));
            if !fonte.is_file() {
                // Se não existe (cenário isolado), cria para calcular a chave.
                self.gerar_fonte_falsa(&format!("mod_{}.lean", i), tamanho_fonte)?;
            }
            let dados = fs::read(&fonte)?;
            let chave = module_key(&dados, &[], &None, &[]);

            if self.store.contains(&chave) {
                let destino = self.tmp_dir.join(format!("restaurado_{}", i));
                let restaurados = self.store.restore(&chave, &destino)?;
                arquivos += restaurados.len();
                hits += 1;
            } else {
                // Miss inesperado neste cenário.
            }
        }

        let duracao = inicio.elapsed();
        Ok(BenchResult {
            nome: format!("build quente ({} módulos)", n_modulos),
            duracao,
            hits,
            misses: 0,
            arquivos_processados: arquivos,
            tamanho_store_bytes: self.store.size_bytes().unwrap_or(0),
        })
    }

    /// Cenário 3: hash puro — mede quanto tempo leva só o SHA-256 sem I/O de artefatos.
    pub fn hash_puro(&self, n_arquivos: usize, tamanho_bytes: usize) -> std::io::Result<BenchResult> {
        let inicio = Instant::now();
        let mut arquivos = 0usize;

        for i in 0..n_arquivos {
            let fonte = self.gerar_fonte_falsa(&format!("hash_{}.lean", i), tamanho_bytes)?;
            let dados = fs::read(&fonte)?;
            let _ = sha256_hex(&dados);
            arquivos += 1;
        }

        let duracao = inicio.elapsed();
        Ok(BenchResult {
            nome: format!("hash puro ({} arquivos)", n_arquivos),
            duracao,
            hits: 0,
            misses: 0,
            arquivos_processados: arquivos,
            tamanho_store_bytes: 0,
        })
    }

    /// Cenário 4: hit parcial (alguns módulos mudam).
    pub fn hit_parcial(&mut self, n_modulos: usize, n_mudados: usize, tamanho_fonte: usize) -> std::io::Result<BenchResult> {
        let inicio = Instant::now();
        let mut hits = 0usize;
        let mut misses = 0usize;
        let mut arquivos = 0usize;

        // Primeiro, build frio de todos.
        for i in 0..n_modulos {
            let fonte = self.gerar_fonte_falsa(&format!("parcial_{}.lean", i), tamanho_fonte)?;
            let dados = fs::read(&fonte)?;
            let chave = module_key(&dados, &[], &None, &[]);
            let artefatos = self.gerar_artefatos_falsos(&fonte)?;
            arquivos += artefatos.len();
            self.store.store(&chave, &artefatos)?;
            misses += 1;
        }

        // Agora, simula que n_mudados mudaram: regenera fonte -> nova chave -> miss.
        for i in 0..n_mudados {
            let fonte = self.gerar_fonte_falsa(&format!("parcial_mudado_{}.lean", i), tamanho_fonte)?;
            let dados = fs::read(&fonte)?;
            let chave = module_key(&dados, &[], &None, &[]);
            let artefatos = self.gerar_artefatos_falsos(&fonte)?;
            arquivos += artefatos.len();
            self.store.store(&chave, &artefatos)?;
            misses += 1;
        }

        // O restante (n_modulos - n_mudados) seria hit se reutilizássemos as chaves antigas.
        // Simula hit para os não-mudados.
        for i in n_mudados..n_modulos {
            let fonte = self.tmp_dir.join(format!("parcial_{}.lean", i));
            if fonte.is_file() {
                let dados = fs::read(&fonte)?;
                let chave = module_key(&dados, &[], &None, &[]);
                if self.store.contains(&chave) {
                    let destino = self.tmp_dir.join(format!("parcial_restaurado_{}", i));
                    let restaurados = self.store.restore(&chave, &destino)?;
                    arquivos += restaurados.len();
                    hits += 1;
                }
            }
        }

        let duracao = inicio.elapsed();
        Ok(BenchResult {
            nome: format!("hit parcial ({} módulos, {} mudados)", n_modulos, n_mudados),
            duracao,
            hits,
            misses,
            arquivos_processados: arquivos,
            tamanho_store_bytes: self.store.size_bytes().unwrap_or(0),
        })
    }
}

impl Default for BenchSuite {
    fn default() -> Self {
        Self::novo().expect("falha ao criar bench suite")
    }
}

/// Imprime um resultado de benchmark formatado.
pub fn imprimir_resultado(r: &BenchResult) {
    let micros = r.duracao.as_micros();
    let millis = r.duracao.as_millis();
    println!("  {}: {:?} ({} ms, {} micros)", r.nome, r.duracao, millis, micros);
    println!("    hits={} misses={} arquivos={} store={} bytes",
        r.hits, r.misses, r.arquivos_processados, r.tamanho_store_bytes);
}

/// Executa a suíte completa e imprime um relatório comparativo.
pub fn executar_suite() -> std::io::Result<()> {
    println!("=== Benchmark do lean-cache (simulado, sem Lean) ===\n");

    // Build frio: 100 módulos, 10 KB cada.
    let mut suite = BenchSuite::novo()?;
    let r1 = suite.build_frio(100, 10_000)?;
    imprimir_resultado(&r1);

    // Build quente: mesmo cenário, tudo hit.
    let r2 = suite.build_quente(100, 10_000)?;
    imprimir_resultado(&r2);

    if r1.duracao.as_nanos() > 0 {
        let speedup = r1.duracao.as_nanos() as f64 / r2.duracao.as_nanos() as f64;
        println!("  Speedup (frio vs quente): {:.2}x", speedup);
    }

    // Hash puro.
    let r3 = suite.hash_puro(1000, 10_000)?;
    imprimir_resultado(&r3);

    // Hit parcial: 100 módulos, 10 mudam.
    let r4 = suite.hit_parcial(100, 10, 10_000)?;
    imprimir_resultado(&r4);

    println!("\n=== Fim do benchmark ===");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_frio_gera_misses() -> std::io::Result<()> {
        let mut suite = BenchSuite::novo()?;
        let r = suite.build_frio(10, 1_000)?;
        assert_eq!(r.misses, 10);
        assert_eq!(r.hits, 0);
        Ok(())
    }

    #[test]
    fn build_quente_gera_hits() -> std::io::Result<()> {
        let mut suite = BenchSuite::novo()?;
        suite.build_frio(10, 1_000)?; // popula
        let r = suite.build_quente(10, 1_000)?;
        assert_eq!(r.hits, 10);
        assert_eq!(r.misses, 0);
        Ok(())
    }

    #[test]
    fn hash_puro_nao_armazena() -> std::io::Result<()> {
        let suite = BenchSuite::novo()?;
        let r = suite.hash_puro(10, 1_000)?;
        assert_eq!(r.tamanho_store_bytes, 0);
        Ok(())
    }
}
