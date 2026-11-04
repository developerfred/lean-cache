# lean-cache — Diagnóstico: o que está feito e o que falta

## Resumo executivo

O projeto compila com zero warnings e já implementa praticamente todas as funcionalidades listadas no README (fases 1–4). O que falta é principalmente:

1. **Conectar comandos listados no `print_usage()` mas ausentes no `match` do `main()`** (`s3-push`, `s3-pull`, `s3-sync`, `snap`, `snap-load`, `snap-list`)
2. **Integrar edição cirúrgica ao `cmd_build`** — hoje ela conta hits/misses mas não afeta a decisão de pular o `lake build`
3. **Integrar snapshots `lean --server` ao protocolo real do Lean**
4. **Conectar workers remotos ao CLI** (módulo `worker.rs` existe mas não é usado)
5. **Testes de integração com projetos Lean reais** (são apenas testes unitários simulados)

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

### A. Comandos documentados no `print_usage()` mas NÃO implementados no `match` do `main()`

Estes aparecem na ajuda (`lean-cache --help`) mas não fazem nada ao digitar:

| Comando | Onde está no código | O que falta |
|---------|---------------------|-------------|
| `s3-push <chave>` | listado no `print_usage` (linha 91) | Ausente no `match` — precisa chamar `push_key_to_s3` de `s3.rs` |
| `s3-pull <chave>` | listado no `print_usage` (linha 92) | Ausente no `match` — precisa chamar `pull_key_from_s3` de `s3.rs` |
| `s3-sync` | listado no `print_usage` (linha 93) | Ausente no `match` — precisa sincronizar chaves locais para S3 |
| `snap <chave>` | listado no `print_usage` (linha 94) | Ausente no `match` — precisa salvar snapshot via `snapshots.rs` |
| `snap-load <chave>` | listado no `print_usage` (linha 95) | Ausente no `match` — precisa restaurar snapshot via `snapshots.rs` |
| `snap-list` | listado no `print_usage` (linha 96) | Ausente no `match` — precisa listar snapshots via `snapshots.rs` |

**Ação:** adicionar esses 6 casos ao `match` em `src/main.rs` (linhas ~1047+).

### B. Edição cirúrgica não integrada ao `cmd_build` (Fase 5)

Hoje `cmd_build` (linhas 505–568 de `main.rs`) calcula `decl_hits`/`decl_misses` mas:

- `all_hit` é definido como `false` se **qualquer** declaração está em miss (linha 554), o que anula o benefício da edição cirúrgica: mesmo que 90% das declarações estejam em cache, se 1 mudou, executa `lake build` inteiro.
- As declarações em hit **não são restauradas** individualmente; apenas o módulo inteiro é restaurado ou recompilado.

**Ação:** modificar `cmd_build` para, quando houver miss parcial, reutilizar declarações cirúrgicas em cache e apenas recompilar as faltantes (ou pelo menos não invalidar o cache do módulo inteiro por causa de uma declaração).

### C. Snapshots `lean --server` não conectados ao protocolo real (Fase 5)

O módulo `snapshots.rs` está completo (store, serialização JSON, chave por módulo+hash+versão), mas:

- Não há integração com `lean --server` para capturar/restaurar o estado de elaboração real.
- Os comandos CLI `snap`, `snap-load`, `snap-list` não existem no `match`.

**Ação:** implementar a ponte entre `snapshots.rs` e o protocolo `lean --server`, e expor via CLI.

### D. Workers remotos não conectados ao CLI

O módulo `worker.rs` está completo (`WorkerPool`, `RemoteWorkerClient`, round-robin, base64), mas:

- Nenhum comando CLI para adicionar/listar/usar workers.
- Não é chamado de nenhum lugar em `main.rs`.

**Ação:** adicionar comandos como `worker-add <url>`, `worker-list`, `worker-build <arquivo.lean>` (ou integrar ao `build` com flag `--worker`).

### E. `remote::pull_key` é um stub

Em `src/remote.rs` (linha 364), `pull_key` retorna `(0, 0)` sem fazer nada. O pull real é feito inline no `cmd_pull` e `cmd_build`.

**Ação:** implementar corretamente ou remover.

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
