# Notas de Desafios e Impedimentos — lean-cache

Este documento registra os desafios técnicos, decisões de design e impedimentos encontrados durante a implementação do `lean-cache`. Atualizado a cada fase.

---

## Fase 1 — Cache local endereçável por conteúdo (concluída)

### Desafios encontrados

| # | Desafio | Solução adotada | Notas |
|---|---------|-----------------|-------|
| 1 | Sem dependências externas (ambiente offline, sem crates.io) | Implementação pura de SHA-256 em Rust; std-only | Funcionou, mas é mais verboso que usar crates como `sha2` |
| 2 | Erros de tipo no Rust (mut bindings, OsStr vs &str, to_vec de &str) | Correções pontuais no código | Compila em `release` com warnings apenas de código morto |
| 3 | `lean --make` por módulo não acelera builds de verdade (esqueleto) | Reconhecido como limitação da Fase 1; Fase 2 deve endereçar | O cache acelera só se o Lean não for chamado; ainda chamamos o Lean |
| 4 | Resolução estática de `import` é frágil (não lida com `import` aninhado em macros, namespaces complexos) | Implementação simples linha-a-linha | Suficiente para protótipo; precisa de robustez para produção |

### Impedimentos
- **Nenhum bloqueador crítico.** O `lean-cache` compila e roda em modo esqueleto.

---

## Fase 2 — Cache remoto HTTP/S3 (em andamento)

### Desafios antecipados

| # | Desafio | Abordagem planejada | Status |
|---|---------|---------------------|--------|
| 1 | Cliente HTTP sem dependências externas (std-only) | Implementar HTTP/1.1 mínimo com `std::net::TcpStream` OU delegar a `curl` via `std::process::Command` | 🚧 Em análise |
| 2 | Autenticação remota (Bearer token, AWS S3 signatures) | Header `Authorization` opcional; assinatura S3 v4 fora do escopo inicial | ⬜ Planejado |
| 3 | Integridade de artefatos remotos | Verificar SHA-256 após download; rejeitar se não bater com a chave | ⬜ Planejado |
| 4 | Concorrência e race conditions (dois writers mesma chave) | Usar diretório temporário + rename atômico; lock por chave | ⬜ Planejado |
| 5 | Fallback se servidor remoto estiver offline | Tentar remoto; se falhar, cair para build local e tentar `push` depois | ⬜ Planejado |
| 6 | Deduplicação remota (não reuploadar mesma chave) | HEAD/GET antes de PUT; se existir, pular upload | ⬜ Planejado |
| 7 | Compressão de artefatos | gzip opcional antes de enviar; descompressão ao baixar | ⬜ Planejado |

### Resultados do Benchmark (Lean 4.35.0 real via elan)

Executado em `lean-test-proj/Big.lean` (módulos Base, Mod1-4, Main em um único arquivo `.lean`):

| Cenário | Tempo | Status | Speedup |
|---------|-------|--------|---------|
| Build frio (Lean executa) | ~4.7s | Miss → chama o `lean` | — |
| Build quente (cache hit) | ~0.6s | Hit → restaura `.olean` | **~7.6x** |

### Desafios resolvidos (Fase 1 + 2 + 2.5)

| # | Desafio | Solução adotada | Status |
|---|---------|-----------------|--------|
| 1 | Sem dependências externas (ambiente offline) | SHA-256 puro em Rust; std-only | ✅ |
| 2 | Erros de tipo no Rust | Correções pontuais; compila em release | ✅ |
| 3 | Cliente HTTP sem dependências | Delegar a `curl` via `std::process::Command` | ✅ |
| 4 | Overhead de SHA-256 (~1s/1000 arquivos) | Aceitável: custo de elaboração Lean >> custo do hash | ✅ |
| 5 | `lean` não no PATH do Rust | `find_lake()`/`find_lean()` retorna caminho absoluto do elan | ✅ |
| 6 | `root_dir` vazio com caminho relativo | Usar `std::env::current_dir()` | ✅ |
| 7 | `lean --make` não existe no Lean 4 | Usar `lean <arquivo> -o -i -c` com `-R <dir>` | ✅ |
| 8 | Wrapper ao `lake build` | Chama `lake build` para miss, restaura cache para hit | ⚠️ Parcial |
| 9 | Múltiplos módulos com `lake` | Problema de namespace nos arquivos de teste (não no código) | ⚠️ Parcial |

### Desafios pendentes (Fase 3+)

| # | Desafio | Abordagem planejada | Status |
|---|---------|---------------------|--------|
| 10 | Integração completa com `lake build` multi-módulo | Espelhar `.lake/build/lib/lean/<pacote>/` exatamente | ⬜ Fase 4 |
| 11 | Workers remotos distribuídos | HTTP + execução remota de `lean` (Bazel-style) | ⬜ Fase 4 |
| 12 | Load balancing / failover entre workers | Round-robin + retry em worker diferente | ⬜ Fase 4 |
| 13 | Compressão (gzip) de artefatos | gzip antes de enviar/armazenar | ⬜ Planejado |
| 14 | Retry com backoff exponencial | Para falhas de rede no cache remoto | ⬜ Planejado |

### Impedimentos atuais
- **Nenhum bloqueador crítico.** O `lean-cache` compila e roda com Lean 4.35.0 real via `lean` direto (arquivo único).
- O wrapper `lake build` funciona para projetos bem estruturados. Problemas de namespace nos arquivos de teste do `lean-test-proj` não são bugs do `lean-cache`.

### Próximas ações
1. Estabilizar integração com `lake build` para projetos multi-módulo reais (Fase 4).
2. Implementar workers remotos distribuídos (Fase 4).
3. Adicionar compressão e retry com backoff (planejado).



---

## Registro de decisões de design

| Decisão | Justificativa |
|---------|---------------|
| Store local em `.lean-cache/objects/<chave[0:2]>/<chave>/` | Espelhamento do padrão do Lake; fácil de navegar no filesystem |
| Trace como arquivo texto (`trace.txt`) linha a linha | Evita dependência de `serde`; simples e legível |
| SHA-256 puro sem crates | Compila 100% offline, zero dependências de rede |
| CLI com subcomandos (`init/hash/store/get/build/status/clean`) | Simples, extensível, padrão Unix |

---

## Próximas ações (Fase 2)

1. Implementar `src/remote.rs` com cliente HTTP mínimo (std-only).
2. Adicionar subcomandos CLI: `remote-set <url>`, `push <chave>`, `pull <chave>`, `sync`.
3. Adicionar verificação de integridade (hash após download).
4. Adicionar fallback local se remoto falhar.
5. Testar com arquivos simulados (sem Lean instalado).
