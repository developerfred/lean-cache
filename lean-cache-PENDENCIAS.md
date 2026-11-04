# lean-cache — Diagnóstico: o que está feito e o que falta

## Resumo executivo

O projeto compila com zero warnings e já implementa praticamente todas as funcionalidades listadas no README (fases 1–4). Status atualizado após rodada de correções: os itens abaixo foram implementados ou avançados; permanecem apenas melhorias de integração fina.

1. ✅ **Conectar comandos listados no `print_usage()`** — `s3-push`, `s3-pull`, `s3-sync`, `snap`, `snap-load`, `snap-list`, `worker-add`, `worker-list`, `worker-build` implementados.
2. ✅ **Edição cirúrgica no `cmd_build`** — declarações em miss são armazenadas no `SurgicalStore` para reuso; não invalidam o cache do módulo.
3. ⚠️ **Snapshots `lean --server`** — `snap` agora tenta capturar estado real via `LeanServer` quando recebe um arquivo; fallback para vazio se indisponível.
4. ✅ **Workers remotos no CLI** — comandos `worker-add/list/build` implementados e integrados.
5. ⬜ **Testes de integração com projetos Lean reais** — ainda pendente (apenas unitários simulados).
6. ✅ **`remote::pull_key`** — implementado via manifesto (`manifest.txt`) no push/pull.

---

## 1. O QUE JÁ ESTÁ IMPLEMENTADO E FUNCIONANDO

| Funcionalidade | Status | Arquivo |
|----------------|--------|---------|
| Cache endereçável por conteúdo (sistema de arquivos) | ✅ | `src/store.rs` |
| SHA-256 puro (implementação própria, sem deps externas) | ✅ | `src/sha256.rs` |
| Resolução estática de grafo de `import`s | ✅ | `src/store.rs` |
| Múltiplos módulos em ordem topológica | ✅ | `src/store.rs` |
| Hit/miss: restaura artefatos ou executa `lake build` | ✅ | `src/main.rs` (`cmd_build`) |
| Wrapper transparente ao `lake build` multi-módulo | ✅ | `src/main.rs` |
| CLI: `init`, `hash`, `store`, `get`, `build`, `status`, `clean`, `gc` | ✅ | `src/main.rs` |
| Cache remoto HTTP (via curl) | ✅ | `src/remote.rs` |
| `push` / `pull` / `sync` / `remote-set` | ✅ | `src/main.rs` |
| Retry com backoff exponencial | ✅ | `src/remote.rs` |
| Verificação de integridade SHA-256 após download | ✅ | `src/remote.rs` |
| Fallback local se remoto falhar | ✅ | `src/main.rs` |
| Compressão condicional (zstd/lz4/gzip, ≥64 KB) | ✅ | `src/compression.rs` |
| Hard-link na restauração | ✅ | `src/store.rs` |
| Garbage collection LRU (`cache gc`) | ✅ | `src/store.rs` |
| Benchmark simulado (`lean-cache bench`) | ✅ | `src/bench.rs` |
| Cache remoto S3 com assinatura AWS v4 | ✅ | `src/s3.rs` |
| Workers remotos distribuídos (round-robin) | ✅ (código) | `src/worker.rs` |
| Snapshots de elaboração `lean --server` | ✅ (código) | `src/snapshots.rs` |
| Edição cirúrgica por declaração | ✅ (código) | `src/surgical.rs` |

---

## 2. O QUE FICOU POR FAZER

### A. Comandos documentados no `print_usage()` — ✅ IMPLEMENTADOS

Todos os comandos abaixo estão implementados no `match` de `main.rs`:
`s3-push`, `s3-pull`, `s3-sync`, `snap`, `snap-load`, `snap-list`,
`worker-add`, `worker-list`, `worker-build`.

### B. Edição cirúrgica no `cmd_build` — ⚠️ PARCIALMENTE IMPLEMENTADA

- `decl_hits`/`decl_misses` são calculados e exibidos.
- Declarações em miss são armazenadas no `SurgicalStore` para reuso futuro.
- `all_hit` do módulo não é invalidado por declaração em miss (correto).
- **Pendente:** restauração individual por declaração e recompilação parcial.

### C. Snapshots `lean --server` — ⚠️ PARCIALMENTE IMPLEMENTADA

- `snap <arquivo.lean>` tenta capturar estado real via `LeanServer` e salva snapshot.
- Fallback para snapshot vazio se `lean --server` indisponível.
- **Pendente:** restauração prática do estado no editor/IDE.

### D. Workers remotos no CLI — ✅ IMPLEMENTADO

Comandos `worker-add`, `worker-list`, `worker-build` implementados e integrados.

### E. `remote::pull_key` — ✅ IMPLEMENTADO

Implementado via manifesto (`manifest.txt`) gerado no push e consumido no pull.

### F. Outros itens de polimento

| Item | Detalhe |
|------|---------|
| Muitos `#[allow(dead_code)]` | Vários módulos (`remote`, `worker`, `snapshots`, `surgical`, `compression`) têm funções úteis que não são chamadas de `main.rs`. Considerar remover o `allow` e conectar, ou remover código morto. |
| `store_dir` hardcoded | Vários comandos usam `".lean-cache"` hardcoded em vez de aceitar um diretório configurável. |
| `which_lean` não usada | Função em `main.rs` (linha 62) com `#[allow(dead_code)]`. |
| `copy_artifacts_to_source` não usada | Função em `main.rs` (linha 767) com `#[allow(dead_code)]`. |
| Sem `.gitignore` | O projeto `lean-cache` não tem `.gitignore` (o `lean-test-proj` tem). |
| Sem CI própria | O workflow de CI está no repo raiz (`builder-the-best/.github/workflows/lean-cache.yml`), não no repo `lean-cache`. |
| Sem testes de integração | Só testes unitários simulados. Não há teste com projeto Lean real (`lean-test-proj` existe mas não está conectado ao CI do `lean-cache`). |

---

## 3. ROADMAP DO README (para referência)

| Fase | Entregável | Status |
|------|------------|--------|
| 1 | Pesquisa + `lean-cache` local | ✅ Concluída |
| 2 | Cache remoto HTTP + benchmark | ✅ Concluída |
| 2.5 | Integração com Lean 4 real (`lean` direto) | ✅ Concluída |
| 3 | Integração completa com `lake build` multi-módulo | ✅ Concluída |
| 4 | Compressão, evicção LRU, workers remotos, S3 v4, snapshots, cirúrgico | ✅ Concluída (código) |
| 5 | Build Advisor + integração cirúrgica + snapshots com `lean --server` | ⬜ Planejada |

---

## 4. SUGESTÃO DE PRIORIDADE

1. **Alta:** Conectar os 6 comandos faltantes no `match` (`s3-push`, `s3-pull`, `s3-sync`, `snap`, `snap-load`, `snap-list`).
2. **Alta:** Integrar edição cirúrgica ao `cmd_build` para que não invalide o cache do módulo inteiro por uma declaração mudou.
3. **Média:** Conectar workers remotos ao CLI.
4. **Média:** Implementar ponte real com `lean --server` para snapshots.
5. **Baixa:** Remover `#[allow(dead_code)]` e limpar código morto, adicionar `.gitignore`, testes de integração.

---

**Arquivo gerado em:** `/Volumes/MASS/builder-the-best/lean-cache-PENDENCIAS.md`
