# Notas de Desafios e Impedimentos — lean-cache

Este documento registra os desafios técnicos, decisões de design e impedimentos encontrados durante a implementação do `lean-cache`. Atualizado a cada fase.

---

## Fase 1 — Cache local endereçável por conteúdo (concluída)

### Desafios encontrados

| # | Desafio | Solução adotada | Notas |
|---|---------|-----------------|-------|
| 1 | Sem dependências externas (ambiente offline, sem crates.io) | Implementação pura de SHA-256 em Rust; std-only | Funcionou, é mais verboso que usar crates como `sha2` |
| 2 | Erros de tipo no Rust (mut bindings, OsStr vs &str, to_vec de &str) | Correções pontuais no código | Compila em `release` com warnings apenas de código morto |
| 3 | `lean --make` por módulo não acelera builds de verdade (esqueleto) | Reconhecido como limitação da Fase 1; Fase 2 endereçou com `lake build` | O cache acelera quando o Lean não é chamado |
| 4 | Resolução estática de `import` é frágil (não lida com `import` aninhado em macros, namespaces complexos) | Implementação simples linha-a-linha | Suficiente para protótipo; precisa de robustez para produção |

### Impedimentos

- **Nenhum bloqueador crítico.** O `lean-cache` compila e roda em modo esqueleto.

---

## Fase 2 — Cache remoto HTTP (concluída)

### Desafios resolvidos

| # | Desafio | Solução adotada | Status |
|---|---------|-----------------|--------|
| 1 | Cliente HTTP sem dependências externas | Delegar a `curl` via `std::process::Command` | ✅ |
| 2 | Autenticação remota (Bearer token) | Header `Authorization` opcional via `RemoteConfig.auth_token` | ✅ |
| 3 | Integridade de artefatos remotos | `download_artifact_verified` com verificação SHA-256 após download | ✅ |
| 4 | Concorrência e race conditions | Diretório temporário com nome não previsível; atomicidade por chave | ✅ |
| 5 | Fallback se servidor remoto offline | `cmd_pull` e `cmd_build` continuam com build local se remoto falhar | ✅ |
| 6 | Deduplicação remota | `key_exists_remote` (HEAD) antes de PUT; `sync` pula chaves existentes | ✅ |
| 7 | Compressão de artefatos | Compressão condicional zstd/lz4/gzip acima de 64 KB | ✅ |
| 8 | Retry com backoff exponencial | `curl_with_retry` com `max_retries` e `backoff_base_secs` configuráveis | ✅ |

### Resultados do Benchmark (Lean 4.35.0 real via elan)

Executado em `lean-test-proj/` (módulos Base, Mod1-4, Main via `lake build`):

| Cenário | Tempo | Status | Speedup |
|---------|-------|--------|---------|
| Build frio (Lean executa, cache vazio) | ~7.2s | Miss → chama o `lake build` | — |
| Build quente (cache hit) | ~0.27s | Hit → restaura `.olean` | **~26x** |
| Build quente (benchmark simulado, 100 módulos) | ~0.33s | Hit → restaura artefatos | **~3.1x** |

---

## Fase 3 — Integração completa com `lake build` multi-módulo (concluída)

### Desafios resolvidos

| # | Desafio | Solução adotada | Status |
|---|---------|-----------------|--------|
| 9 | Wrapper `lake build` para projetos multi-módulo | `cmd_build` espelha `.lake/build/lib/lean/<pacote>/` exatamente | ✅ |
| 10 | Múltiplos módulos com namespace | `restore_artifacts_to_lake` resolve `Testproj.Mod1` → `Testproj/Mod1.olean` | ✅ |
| 11 | Detecção automática de artefatos | `collect_lake_artifacts` varre recursivamente `.olean`, `.ilean`, `.c`, `.o` | ✅ |
| 12 | Hard-link na restauração | Evita cópia de disco quando possível; fallback para `fs::copy` | ✅ |

---

## Fase 4 — Compressão, Evicção e Workers Remotos (concluída)

### Desafios resolvidos

| # | Desafio | Solução adotada | Status |
|---|---------|-----------------|--------|
| 13 | Compressão de artefatos (zstd/lz4) | Módulo `compression.rs` com zstd, lz4 e gzip fallback | ✅ |
| 14 | Evicção de cache (LRU) | `cache_gc()` remove entradas menos acessadas quando excede `max_store_bytes` | ✅ |
| 15 | Workers remotos distribuídos | `worker.rs` com `WorkerPool` (round-robin) e `RemoteWorkerClient` (HTTP via curl) | ✅ |
| 16 | Cache remoto S3 com assinatura v4 | `s3.rs` com assinatura AWS v4, validação de path traversal, credenciais redacted | ✅ |
| 17 | Snapshots de elaboração `lean --server` | `snapshots.rs` com `SnapshotStore` e `ElaborationSnapshot` | ✅ |
| 18 | Edição cirúrgica | `surgical.rs` com `SurgicalStore` e extração de declarações individuais | ✅ |

### Segurança implementada

| # | Medida | Descrição |
|---|--------|-----------|
| S1 | Validação de chave de cache | `validate_cache_key()` rejeita `..`, `/` inicial, byte nulo |
| S2 | Validação de chave S3 | `validate_s3_key()` rejeita path traversal em chaves S3 |
| S3 | Validação de URL | `extract_host()`/`extract_path()` só aceitam http:// ou https:// |
| S4 | Validação de módulo | `validate_module_name()` rejeita caracteres inválidos e path traversal |
| S5 | Arquivos temporários não previsíveis | `temp_filename_secure()` usa hash de tempo + PID |
| S6 | Credenciais redacted | `Debug` de `S3Config` oculta `access_key` e `secret_key` |
| S7 | Não logar URLs/stderr sensíveis | Erros de rede mostram apenas status code, não conteúdo |

---

## Fase 5 — Build Advisor e edição cirúrgica (planejada)

| # | Desafio | Abordagem planejada | Status |
|---|---------|---------------------|--------|
| 19 | Build Advisor (WSP direction) | LLM lê grafo de dependências + traces; recomenda flags/ordem | ⬜ Pesquisa futura |
| 20 | Edição cirúrgica por declaração | `surgical.rs` implementado; integração com `lake build` pendente | ⚠️ Parcial |
| 21 | Estado de elaboração entre sessões `lean --server` | `snapshots.rs` implementado; integração com `lean --server` pendente | ⚠️ Parcial |

---

## Registro de decisões de design

| Decisão | Justificativa |
|---------|---------------|
| Store local em `.lean-cache/objects/<chave[0:2]>/<chave>/` | Espelhamento do padrão do Lake; fácil de navegar no filesystem |
| Chave de conteúdo = SHA-256(`fonte` + `import_keys` + `lean_version` + `flags`) | Determinístico; imports transitivos resolvidos recursivamente |
| Compressão condicional (zstd/lz4, ≥64 KB) | Evita overhead de compressão em arquivos pequenos; reduz disco/banda em grandes |
| Hard-link na restauração | Evita cópia de disco quando possível; fallback para `fs::copy` |
| CLI com subcomandos | Simples, extensível, padrão Unix |
| `curl` via `std::process::Command` | Compila 100% offline, zero dependências de rede |
| AWS Signature v4 para S3 | Autenticação padrão da AWS para buckets privados |

---

## Próximas ações

1. Integração da edição cirúrgica (`surgical.rs`) com `cmd_build` para cache por declaração
2. Integração de snapshots (`snapshots.rs`) com `lean --server` para estado de elaboração persistente
3. Testes de integração com S3 real (MinIO ou AWS)
4. Build Advisor (direção WSP — pesquisa futura)
5. Testes automatizados no CI para todos os subcomandos

---

## Registro de métricas

| Métrica | Valor |
|---------|-------|
| Speedup real (Lean 4.35.0, `lean-test-proj`) | ~26x |
| Speedup simulado (bench, 100 módulos) | ~3.1x |
| Compressão zstd (arquivos ≥64 KB) | Redução de tamanho variável |
| Evicção LRU | Disponível via `lean-cache gc` |
