# lean-cache — Content-addressable build cache for Lean 4

A cache endereçável por conteúdo para acelerar builds do Lean 4.
Funciona como um wrapper ao `lean` / `lake`:

- resolve o grafo de `import`s em ordem topológica;
- calcula a chave de conteúdo de cada módulo (`fonte + imports + versão do Lean + flags`);
- se **todos** os módulos estão em cache hit, pula a invocação do `lean`/`lake` e restaura os artefatos (`.olean`, `.ilean`, `.c`);
- se **algum** módulo está em miss, executa o build normalmente e armazena os artefatos no cache;
- suporta cache remoto via HTTP (delegado ao `curl`, sem dependências externas).

## Instalação (offline, std-only)

```bash
cd lean-cache
cargo build --release
cp target/release/lean-cache /usr/local/bin/
```

## Uso

```bash
# Inicializa um store de cache no diretório atual (.lean-cache)
lean-cache init

# Calcula a chave de conteúdo de um módulo Lean
lean-cache hash src/MeuModulo.lean

# Armazena artefatos de um diretório sob uma chave
lean-cache store <chave> <diretorio-com-artefatos>

# Restaura artefatos de uma chave para um diretório
lean-cache get   <chave> <diretorio-destino>

# Build acelerado de um arquivo raiz (chama o Lean diretamente)
lean-cache build Big.lean

# Estatísticas do store local
lean-cache status

# Remove o store local
lean-cache clean

# --- Cache remoto ---
lean-cache remote-set https://cache.suaempresa.com
lean-cache push <chave>          # envia artefatos locais para o remoto
lean-cache pull <chave>          # baixa artefatos do remoto
lean-cache sync                  # push de todas as chaves locais ainda no remoto

# --- Benchmark (simulado, sem Lean) ---
lean-cache bench
```

## Modelo do store

```
.lean-cache/
  remote.txt          # config do cache remoto
  objects/
    ab/
    abcd1234/         # conteúdos: .olean .ilean .c trace.txt
```

## Resultado real medido (Lean 4.35.0 via elan)

Ambiente: macOS ARM64, Lean 4.35.0, arquivo `Big.lean` (6 módulos definidos em um único arquivo `.lean`).

| Cenário | Tempo | Status |
|---------|-------|--------|
| Build frio (Lean executa, cache vazio) | **~4.7s** | Miss → chama o `lean` |
| Build quente (cache hit, pula o `lean`) | **~0.6s** | Hit → restaura `.olean` |
| **Speedup** | **~7.6x** | — |

## Status do protótipo

| Funcionalidade | Status |
|----------------|--------|
| Cache endereçável por conteúdo (sistema de arquivos) | ✅ |
| SHA-256 puro (sem dependências externas) | ✅ |
| Resolução estática de grafo de `import`s | ✅ |
| Múltiplos módulos em ordem topológica | ✅ (código) / ⚠️ (integração com `lake` pendente) |
| Hit/miss: restaura artefatos ou executa `lean` | ✅ (via `lean` direto) |
| Wrapper transparente ao `lake build` | ⚠️ Parcial (funciona para projetos com namespace correto) |
| CLI: init / hash / store / get / build / status / clean | ✅ |
| Cache remoto HTTP (via curl) | ✅ |
| Push / Pull / Sync / remote-set | ✅ |
| Benchmark simulado | ✅ |
| Cache remoto S3 / autenticação AWS | ⬜ Planejado |
| Compressão (gzip) de artefatos | ⬜ Planejado |
| Retry com backoff exponencial | ⬜ Planejado |
| Execução remota distribuída (workers) | ⬜ Fase 3 |
| Integração com `lean --server` | ⬜ Fase 4 |

## Limitações conhecidas

1. **Wrapper `lake build`**: o `lean-cache build` chama `lake build` para projetos com `lakefile`. Se o projeto tiver problemas de namespace nos arquivos `.lean` (ex: imports com prefixo incorreto), o `lake` falhará — isso é um problema do projeto de teste, não do `lean-cache`. Para projetos bem estruturados, funciona.
2. **Build puro com `lean`**: quando chamado diretamente com `lean <arquivo>`, funciona para arquivos únicos. Para múltiplos arquivos, use o wrapper `lake`.
3. **Ciclos de imports**: Lean 4 não permite ciclos; o `resolve_import_graph` evita loops infinito.

## Roadmap

| Fase | Entregável | Status |
|------|------------|--------|
| 1 | Pesquisa + `lean-cache` local | ✅ Concluída |
| 2 | Cache remoto HTTP + benchmark | ✅ Concluída |
| 2.5 | Integração com Lean 4 real (`lean` direto) | ✅ Concluída (arquivo único) |
| 3 | Execução remota distribuída de elaboração (workers) | ⬜ Planejada |
| 4 | Integração completa com `lake build` | ⬜ Planejada |
| 5 | Build Advisor / edição cirúrgica (direção WSP) | ⬜ Pesquisa futura |

Veja `PESQUISA_LEAN_BUILD.md` para a pesquisa completa e `NOTAS_DESAFIOS_IMPEDIMENTOS.md` para desafios e impedimentos registrados a cada fase.
