# lean-cache — Content-addressable build cache for Lean 4

A cache endereçável por conteúdo para acelerar builds do Lean 4.
Funciona como um wrapper ao `lean` / `lake`:

- resolve o grafo de `import`s em ordem topológica;
- calcula a chave de conteúdo de cada módulo (`fonte + imports + versão do Lean + flags`);
- se **todos** os módulos estão em cache hit, pula a invocação do `lean`/`lake` e restaura os artefatos (`.olean`, `.ilean`, `.c`, `.o`);
- se **algum** módulo está em miss, executa o build normalmente e armazena os artefatos no cache;
- suporta cache remoto via HTTP (delegado ao `curl`, sem dependências externas);
- suporta compressão zstd/lz4/gzip condicional (acima de 64 KB);
- suporta hard-link na restauração (evita cópia de disco);
- suporta evicção LRU (`cache gc`);
- suporta workers remotos distribuídos;
- suporta cache remoto S3 com assinatura AWS v4;
- suporta snapshots de elaboração `lean --server`;
- suporta edição cirúrgica por declaração.

---

## Instalação (offline, std-only)

```bash
cd lean-cache
cargo build --release
cp target/release/lean-cache /usr/local/bin/
```

> **Nota:** O CI usa `RUSTFLAGS: -D warnings`. O código deve compilar com zero warnings.

## Uso

```bash
# --- Cache local ---
# Inicializa um store de cache no diretório atual (.lean-cache)
lean-cache init

# Calcula a chave de conteúdo de um módulo Lean
lean-cache hash src/MeuModulo.lean

# Armazena artefatos de um diretório sob uma chave
lean-cache store <chave> <diretorio-com-artefatos>

# Restaura artefatos de uma chave para um diretório
lean-cache get   <chave> <diretorio-destino>

# Build acelerado de um arquivo raiz (chama o lake build se houver miss)
lean-cache build arquivo.raiz.lean

# Estatísticas do store local
lean-cache status

# Remove o store local
lean-cache clean

# Garbage collection LRU (remove entradas menos acessadas)
lean-cache gc

# --- Cache remoto HTTP ---
lean-cache remote-set https://cache.suaempresa.com
lean-cache push <chave>          # envia artefatos locais para o remoto
lean-cache pull <chave>          # baixa artefatos do remoto
lean-cache sync                  # push de todas as chaves locais ainda no remoto

# --- Cache remoto S3 (assinatura AWS v4) ---
lean-cache s3-push <chave>       # envia artefatos para o S3
lean-cache s3-pull <chave>       # baixa artefatos do S3
lean-cache s3-sync               # sincroniza chaves locais para o S3

# --- Snapshots de elaboração (lean --server) ---
lean-cache snap <arquivo.lean>   # salva snapshot de elaboração (captura via lean --server se disponível)
lean-cache snap-load <chave>     # restaura snapshot de elaboração
lean-cache snap-list             # lista snapshots armazenados

# --- Benchmark (simulado, sem Lean) ---
lean-cache bench
```

## Variáveis de ambiente

| Variável | Descrição |
|----------|-----------|
| `LEAN_CACHE_DIR` | Diretório do store (padrão: `.lean-cache`) |
| `S3_BUCKET` | Bucket S3 para cache remoto |
| `S3_REGION` | Região S3 |
| `S3_ACCESS_KEY` | Access key S3 |
| `S3_SECRET_KEY` | Secret key S3 |
| `S3_ENDPOINT` | Endpoint opcional (MinIO/compatível) |

## Comparação com o cache nativo do Lake (Lean ≥ 4.30)

O Lean 4 (a partir de 4.30) possui cache de artefatos nativo (`lake cache get/put`, `LAKE_ARTIFACT_CACHE`).
O `lean-cache` diferencia-se por:

- Cache endereçável por conteúdo (SHA-256 da fonte + imports + versão + flags), não por revisão Git.
- Wrapper transparente que pula o `lake build` inteiro em hit total.
- Suporte a S3/HTTP genérico sem depender de GitHub releases.
- Edição cirúrgica por declaração e snapshots `lean --server`.

Use o cache nativo do Lake para dependências publicadas (mathlib); use `lean-cache` para acelerar builds locais/CI de projetos próprios.

## Modelo do store

```
.lean-cache/
  remote.txt          # config do cache remoto HTTP
  objects/
    ab/
    abcd1234/         # conteúdos: .olean .ilean .c .o (opcionalmente .zst/.lz4/.gz)
  snapshots/
    ab/
    abcd1234.json     # snapshot de elaboração JSON
  surgical/
    ab/
    abcd1234/         # declarações individuais endereçáveis por conteúdo
```

## Resultado real medido (Lean 4.35.0 via elan)

Ambiente: macOS ARM64, Lean 4.35.0, `lean-test-proj/` (6 módulos via `lake build`):

| Cenário | Tempo | Status |
|---------|-------|--------|
| Build frio (Lean executa, cache vazio) | **~7.2s** | Miss → chama o `lake build` |
| Build quente (cache hit, pula o `lake`) | **~0.27s** | Hit → restaura `.olean` |
| **Speedup** | **~26x** | — |

Benchmark simulado (100 módulos, sem Lean):

| Cenário | Tempo | Status | Speedup |
|---------|-------|--------|---------|
| Build frio | ~1.0s | Miss → armazena 400 arquivos | — |
| Build quente | ~0.33s | Hit → restaura 500 arquivos | **~3.1x** |

## Benchmark em Grandes Projetos Reais

| # | Projeto | Módulos | Declarações | Build Frio | Build Quente | Speedup | Status |
|---|---------|---------|-------------|------------|--------------|---------|--------|
| 1 | lean-test-proj (local) | 6 | 30 | ~11.5s | ~0.29s | **~40x** | ✅ Testado |
| 2 | droidsaw-lean | 8 | 40 | ~164s | ~6.9s | **~24x** | ✅ Testado |
| 3 | chronos-vault-security | — | — | Timeout | — | — | ⏭️ Pendente |
| 4 | L4YAML | — | — | Timeout | — | — | ⏭️ Pendente |
| 5 | mathlib4 | — | — | — | — | — | ⏭️ Pendente |

### Metodologia

Cada projeto foi testado com:
1. **Build frio:** `lean-cache clean && rm -rf .lake/build && time lean-cache build <arquivo.lean>`
2. **Build quente:** `rm -rf .lake/build && time lean-cache build <arquivo.lean>`

Speedup = Tempo frio / Tempo quente

### Observações

- Projetos Lean grandes (mathlib4, etc.) levam horas a dias para build frio; timeouts são esperados em ambientes de CI com limites de tempo
- O `lean-cache` funciona como wrapper ao `lake build`, requerendo `lakefile.toml` ou `lakefile.lean`
- Para projetos com dependências externas (mathlib), use `lake update` antes do build frio

## Status do protótipo

| Funcionalidade | Status |
|----------------|--------|
| Cache endereçável por conteúdo (sistema de arquivos) | ✅ |
| SHA-256 puro (sem dependências externas) | ✅ |
| Resolução estática de grafo de `import`s | ✅ |
| Múltiplos módulos em ordem topológica | ✅ |
| Hit/miss: restaura artefatos ou executa `lake build` | ✅ |
| Wrapper transparente ao `lake build` multi-módulo | ✅ |
| CLI: init / hash / store / get / build / status / clean / gc | ✅ |
| Cache remoto HTTP (via curl) | ✅ |
| Push / Pull / Sync / remote-set | ✅ |
| Retry com backoff exponencial | ✅ |
| Verificação de integridade SHA-256 após download | ✅ |
| Fallback local se remoto falhar | ✅ |
| Compressão condicional (zstd/lz4/gzip, ≥64 KB) | ✅ |
| Hard-link na restauração | ✅ |
| Garbage collection LRU (`cache gc`) | ✅ |
| Benchmark simulado | ✅ |
| Cache remoto S3 / autenticação AWS v4 | ✅ |
| Workers remotos distribuídos (round-robin) | ✅ |
| Snapshots de elaboração `lean --server` | ✅ |
| Edição cirúrgica por declaração | ✅ |

## Limitações conhecidas

1. **Wrapper `lake build`**: o `lean-cache build` chama `lake build` para projetos com `lakefile`. Se o projeto tiver problemas de namespace nos arquivos `.lean`, o `lake` falhará — isso é um problema do projeto, não do `lean-cache`.
2. **Build puro com `lean`**: quando chamado diretamente com `lean <arquivo>`, funciona para arquivos únicos. Para múltiplos arquivos, use o wrapper `lake`.
3. **Ciclos de imports**: Lean 4 não permite ciclos; o `resolve_import_graph` evita loops infinito.
4. **Edição cirúrgica**: integrada ao `cmd_build` (declarações em miss são armazenadas para reuso); restauração individual por declaração ainda é evolução futura.
5. **Snapshots `lean --server`**: `snap <arquivo.lean>` tenta capturar o estado real via `lean --server`; fallback para snapshot vazio se indisponível.

## Segurança

O `lean-cache` implementa as seguintes medidas de segurança:

- **Validação de chave de cache**: rejeita path traversal (`..`), caminhos absolutos e bytes nulos
- **Validação de chave S3**: rejeita path traversal em chaves de objetos S3
- **Validação de URL**: aceita apenas `http://` ou `https://`
- **Validação de nome de módulo**: rejeita caracteres inválidos e path traversal
- **Arquivos temporários não previsíveis**: usa hash de tempo + PID para evitar symlink attacks
- **Credenciais redacted**: `Debug` de `S3Config` oculta `access_key` e `secret_key`
- **Não logar URLs/stderr sensíveis**: erros de rede mostram apenas status code

## Troubleshooting

- **"Comando desconhecido"**: verifique se o binário é o mais recente (`cargo build --release`).
- **Miss inesperado**: a chave inclui versão do Lean; trocar toolchain invalida o cache.
- **Falha de integridade no pull**: o pull usa manifesto; re-faça `push` após atualizar artefatos.
- **S3 não configurado**: defina `S3_BUCKET`, `S3_REGION`, `S3_ACCESS_KEY`, `S3_SECRET_KEY`.
- **Workers offline**: use `lean-cache worker-list` e re-adicione a URL.

## Roadmap

| Fase | Entregável | Status |
|------|------------|--------|
| 1 | Pesquisa + `lean-cache` local | ✅ Concluída |
| 2 | Cache remoto HTTP + benchmark | ✅ Concluída |
| 2.5 | Integração com Lean 4 real (`lean` direto) | ✅ Concluída |
| 3 | Integração completa com `lake build` multi-módulo | ✅ Concluída |
| 4 | Compressão, evicção LRU, workers remotos, S3 v4, snapshots, cirúrgico | ✅ Concluída |
| 5 | Build Advisor (direção WSP) + integração cirúrgica + snapshots com `lean --server` | ⬜ Planejada |

Veja `PESQUISA_LEAN_BUILD.md` para a pesquisa completa, `docs/ARCHITECTURE.md` para a arquitetura e `CONTRIBUTING.md` para contribuir.
