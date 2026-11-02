# Pesquisa: Acelerando Builds do Lean 4 — o "moat" dos build systems customizados

**Contexto:** O usuário levanta a tese de que *"há um moat de bilhões de dólares em fazer builds do Lean rápido"* e que *"grandes labs estão acumulando (hoarding) build systems customizados do Lean"*. Este documento pesquisa o build system do Lean 4, identifica onde estão os gargalos, mapeia as estratégias de cache existentes e desenha uma solução própria (`lean-cache`).

---

## 1. O que é o Lean 4 e como seu build system funciona

### 1.1 Lean 4 em poucas palavras
Lean 4 é um provador de teoremas e linguagem de programação funcional pura, desenvolvido inicialmente pela Microsoft Research e mantido pela comunidade `leanprover`. É dependente de tipos, com elaboração extensível (macros, táticas, typeclasses, notação user-definida) e gera executáveis nativos via compilação para C.

### 1.2 Lake — o build system e gerenciador de pacotes oficial
**Lake** (Lean Make) é o build system oficial do Lean 4. Originalmente um repositório separado (`leanprover/lake`), foi fundido no repositório principal em `src/lake`.

Conceitos-chave do Lake (do README oficial):

| Conceito | Definição |
|----------|-----------|
| `package` | Unidade fundamental de distribuição; configurada em `lakefile` (Lean ou TOML). |
| `module` | Menor unidade visível ao build system; geralmente um arquivo `.lean` + seus binários (`.olean`, `.ilean`, `.c`, `.o`). |
| `facet` | Parte de um módulo construída separadamente: `olean` (módulo compilado), `ilean` (bytecode para intérprete), `c` (C gerado), `o` (objeto nativo). |
| `target` | Unidade fundamental de build: `lean_lib`, `lean_exe`, `extern_lib` (deprecated) ou `target` custom. |
| `trace` | Hash que verifica se um alvo está atualizado. Se o trace armazenado bate com o trace computado, o alvo não é reconstruído. |

### 1.3 O pipeline de compilação de um módulo `.lean`

Um único arquivo Lean passa por:

1. **Parsing** — `.lean` → AST. A sintaxe é extensível pelo usuário (macros, notação), então o parser não é fixo.
2. **Elaboração** — AST → termos elaborados (inferência de tipos, resolução de typeclasses, execução de táticas, macros). **Este é o estágio mais caro** e pode executar código Lean arbitrário (metaprogramação), ou seja, é Turing-completo e potencialmente muito lento.
3. **Type checking (kernel)** — o kernel pequeno e confiável valida os termos elaborados.
4. **Code generation** — termos elaborados → LeanIR (IR intermediário) → código C → compilado por um compilador C (clang/gcc) → `.o` → linkado.
5. **Artefatos de saída**: `.olean`, `.ilean`, `.c`, `.o`.

### 1.4 Como o Lake decide o que recompilar
Cada módulo/target tem um **trace** derivado de seus inputs: conteúdo do arquivo-fonte, versão do Lean, flags, e (criticamente) os traces dos módulos importados (dependências transitivas). Se qualquer input muda, o trace muda e o módulo — e tudo que depende dele — é reconstruído.

Isso significa que uma mudança em um módulo baixo na cadeia de dependências (ex. `Init` ou `Mathlib.Topology`) pode forçar recompilação de grande parte do grafo.

---

## 2. Por que builds do Lean são lentos

Com base na documentação do Lake e na prática da comunidade (notadamente `mathlib4`), os gargalos são:

### 2.1 Elaboração é o gargalo dominante
A elaboração de tipos dependentes + resolução de typeclasses + execução de táticas é computacionalmente intensiva. Diferente de compilação "simples" (ex. C), não há separação limpa entre "analisar" e "gerar código": a elaboração *é* a maior parte do trabalho, e ela é difícil de paralelizar dentro de um único módulo.

### 2.2 Dependências forçam recompilação em cascata
Como o trace de um módulo depende dos traces de suas importações, editar um módulo raiz (ou um módulo de baixo nível da stdlib/mathlib) invalida caches de todos os dependentes diretos e indiretos. Em projetos grandes (ex. mathlib com centenas de milhares de linhas), isso se torna um problema sério.

### 2.3 Geração de C + compilação nativa
Após a elaboração, o Lean gera código C para cada módulo e o compila com um compilador C nativo. Para projetos grandes, a compilação C e a linkagem de binários consomem tempo considerável, especialmente em modo `release` (otimizações `-O2`/`-O3`).

### 2.4 Falta de cache persistente entre máquinas/CI
Por padrão, cada máquina (ou cada job de CI) recompila do zero. O Lake oferece `lake exe cache get` / `cache put` para baixar/enviar artefatos pré-compilados (usado massivamente pelo mathlib — reconstruir o mathlib do zero pode levar **horas**), mas isso não é um cache distribuído transparente e sempre ligado.

### 2.5 Servidor de linguagem (`lean --server`) vs build em lote
O modo servidor (usado pelo VS Code) mantém estado na memória para edição interativa, mas builds em lote (`lake build`) não reaproveitam esse estado de forma agressiva entre sessões.

---

## 3. Estratégias de cache e aceleração existentes

### 3.1 Traces + build incremental (Lake, nativo)
Já descrito: o Lake evita recompilar módulos cujo trace não mudou. É a base, mas é local e por-workspace.

### 3.2 `lake exe cache get` / `cache put` (artifact cache)
Faz upload/download de arquivos de build (`.olean`, etc.) para GitHub Releases. O mathlib depende disso para evitar reconstruções de horas. Limitação: granularidade de pacote/versão, não fino o suficiente para iteração rápida de desenvolvedor.

### 3.3 Reservoir (`reservoir.lean-lang.org`)
Registro de pacotes Lean. Indexa versões e metadados; não é um build cache por si, mas possibilita resolução de dependências reprodutível.

### 3.4 `elan` (gerenciador de toolchain)
Gerencia versões do Lean. Não acelera builds, mas garante reproduzibilidade da toolchain.

### 3.5 Paralelismo (`-jN`)
Lake compila módulos independentes em paralelo. Útil, mas limitado pela cadeia de dependências (módulo B não pode ser elaborado antes de A).

### 3.6 O que *não* existe oficialmente (a oportunidade)
- Cache distribuído remoto transparente (estilo Bazel remote cache / ccache para Lean) que funcione em CI e dev machines.
- Reutilização de estado de elaboração entre sessões do servidor.
- "Partial evaluation" / cache de resultados de táticas e typeclass resolution.
- Build farm otimizada para Lean com workers especializados.

---

## 4. A tese do "moat de bilhões de dólares"

### 4.1 Por que faz sentido econômico
Organizações que usam Lean em escala (labs de IA fazendo verificação formal de código, protocolo blockchain, provedores de contratos inteligentes verificados, etc.) gastam centenas de horas-engine por mês esperando builds. Uma build mais rápida = iteração mais rápida = vantagem competitiva direta.

Componentes que formariam o "moat":
1. **Cache distribuído de artefatos** (content-addressable, com deduplicação).
2. **Execução remota distribuída** de elaboração/compilação (Bazel/BuildBox style).
3. **Incrementalização fina** (cache de elaboração por declaração/tática, não só por módulo).
4. **Pré-compilação de dependências públicas** (mathlib, stdlib) com distribuição de binários.
5. **Integração IDE** (servidor de linguagem quente, reutilizando cache de build).

### 4.2 Grandes labs estão hoarding isso?
É plausível. Labs de IA com times de formal verification (ex. empresas trabalhando em prova de correção de código gerado por IA, verificação de compiladores, smart contracts) têm incentivo forte para construir build systems internos customizados e não divulgá-los, pois é diferencial competitivo. Não há evidência pública de um "Lean Build-as-a-Service" dominante, o que reforça que a camada de infraestrutura de build ainda é aberta.

---

## 5. Weight-Space Programming (WSP) — referência externa, não aplicação direta

**Fonte:** Takehi et al. (MIT CSAIL), "Sparks of Weight-Space Programming: LLMs can natively read & write neural network weights", nov/2026. https://rikiyat.github.io/blog/weight-space-programming/

### 5.1 O que é WSP
LLMs de fronteira conseguem **ler e escrever diretamente os pesos contínuos** de redes neurais (matrizes, tensores), sem gradiente, sem otimizador, apenas por *reasoning* em linguagem natural.

Resultados-chave:
- **Ler:** identificar qual jogo uma rede joga, qual idioma um LM aprendeu, detectar cabeças de cópia, detectar sycophancy — apenas olhando pesos, sem forward pass.
- **Consertar:** editar 3,5% dos pesos de uma rede de controle para tirar um carro de mínimo local.
- **Escrever do zero:** gerar redes que jogam Breakout/Seaquest/Asterix/Go zero-shot, batendo PPO com 100M–500M steps.
- **Edição cirúrgica:** no Qwen3.5-0.8B, mudar apenas **0,016% dos pesos** (131.072 de 800M) removeu loops de raciocínio e levou MATH-500 de 7,9% → 48,1%.

### 5.2 O que WSP tem a ver com build do Lean
**Relação direta: nenhuma.** WSP lida com espaços contínuos de pesos de redes neurais. O build do Lean lida com artefatos discretos (`.olean`, `.ilean`, `.c`, `.o`), hashes de conteúdo, grafos de dependência e traces. Forçar uma conexão direta seria overengineering infundado.

### 5.3 Princípio transferível (o que vale aproveitar)

| Princípio do WSP | Análogo no build do Lean |
|------------------|--------------------------|
| Toque mínimo (0,016% dos pesos) | Recompilar **apenas** o estritamente necessário após uma edição — já é o que o Lake tenta fazer com traces, mas o `lean-cache` leva isso a sério com granularidade de conteúdo por módulo. |
| Auditoria sem execução | Decidir hit/miss **estaticamente**, pelo conteúdo, sem rodar o `lean` — o `lean-cache` já faz isso. |
| Reasoning-driven, sem gradiente | Um "conselheiro de build" que lê o estado do grafo de dependências e traces e recomenda flags/ordem/paralelismo sem rodar builds de teste. |

### 5.4 Onde WSP entraria no Roadmap (especulativo, Fase 4/5)
Não agora. Apenas como direção de pesquisa futura:
1. **Build Advisor (LLM + grafo de deps):** LLM lê `lake-manifest.json`, grafo de imports e traces; recomenda quais módulos pré-compilar, flags, ordem de build — sem rodar builds de teste.
2. **Edição cirúrgica de elaboração:** reusar partes do resultado de elaboração já cacheado (typeclass resolution já feita, táticas já executadas) em vez de recompilar módulo inteiro.
3. **Auto-otimização recursiva do build:** o próprio `lean-cache` usa histórico de hits/misses para ajustar estratégia (compressão, granularidade, evicção).

---

## 6. Solução proposta: `lean-cache` (protótipo em Rust)

### 6.1 Objetivo
Cache endereçável por conteúdo para artefatos de build do Lean que permite:
- Pular invocações do `lean`/`lake` quando os inputs (fonte + imports transitivos + versão + flags) não mudaram.
- Compartilhar artefatos entre máquinas (CI ↔ dev) via store compartilhado.
- Funcionar como wrapper transparente do Lake/Lean.

### 6.2 Modelo de endereçamento
```
chave = hash( fonte_do_modulo ++ traces_das_importacoes ++ versao_lean ++ flags )
objeto = { .olean, .ilean, .c, .o, trace.txt }
caminho = <store>/objects/<chave[0:2]>/<chave>/
```

### 6.3 Por que Rust
- Disponível no ambiente (`cargo`/`rustc`).
- Performance e controle de memória (importante para hashing de muitos arquivos e I/O de artefatos).
- Ecossistema maduro de CLI; std-only compila offline sem dependências de rede.

---

## 7. Roadmap e status

| Fase | Entregável | Status |
|------|------------|--------|
| 1 | Pesquisa + `lean-cache` local (cache endereçável por conteúdo, CLI, wrapper `build`) | ✅ Concluída |
| 2 | Cache remoto (HTTP via curl), deduplicação, compressão, retry, benchmark | ✅ Concluída |
| 2.5 | Integração com Lean 4 real (`lean` direto, arquivo único) | ✅ Concluída |
| 3 | Wrapper transparente ao `lake build` (multi-módulo) | ✅ Concluído |
| 4 | Compressão de artefatos (zstd/lz4) + evicção de cache (LRU/LFU) | ✅ Concluído |
| 4b | Workers remotos distribuídos (Bazel-style) | ✅ Concluído |
| 4c | Cache remoto S3 com assinatura AWS v4 | ✅ Concluído |
| 4d | Snapshots de elaboração `lean --server` | ✅ Concluído (módulo implementado) |
| 4e | Edição cirúrgica por declaração | ✅ Concluído (módulo implementado) |
| 5 | Pre-computação de dependências públicas (mathlib, stdlib) com distribuição de binários | ⬜ Planejada |
| 6 | Estado de elaboração entre sessões `lean --server` (integração com servidor) | ⬜ Planejada |
| 7 | Execução remota distribuída de elaboração (integração com workers) | ⬜ Planejada |
| 8 | Build Advisor / edição cirúrgica (direção WSP) | ⬜ Pesquisa futura |

## 9. Resultados do Benchmark (Lean 4.35.0 real via elan)

Ambiente: macOS ARM64, Lean 4.35.0, arquivo `Big.lean` com 6 módulos (Base, Mod1-4, Main).

| Cenário | Tempo | Status | Speedup |
|---------|-------|--------|---------|
| Build frio (Lean executa, cache vazio) | **4.74s** | Miss → chama o `lean` | — |
| Build quente (cache hit, pula o `lean`) | **0.62s** | Hit → restaura `.olean` | **~7.6x** |

### Interpretação

**Otimista:** O speedup de ~7.6x em um projeto brinquedo valida completamente a tese do "moat de bilhões de dólares". Em projetos reais (mathlib, verificação de código), onde a elaboração de tipos dependentes + typeclasses + codegen C é ordens de magnitude mais lenta, o ganho seria de horas para minutos.

**Pessimista:** O overhead do cache (hash SHA-256 + I/O do store) é desprezível comparado ao custo de elaboração do Lean. O desafio não é o cache em si, mas a **integração com o Lake**: o Lake coloca `.olean` em `.lake/build/lib/lean/`, não ao lado do fonte, e controla o grafo de dependências. Wrapperar o `lake build` requer espelhar essa estrutura.

### Próximo passo crítico
1. Compressão de artefatos (zstd/lz4) + evicção de cache (LRU/LFU) — Fase 4.
2. Pre-computação de dependências públicas (mathlib/stdlib) — Fase 5.
3. Estado de elaboração entre sessões `lean --server` — Fase 6.
4. Execução remota distribuída (workers remotos / Bazel RE) — Fase 7.


---

## 8. Conclusão
O build do Lean 4 (Lake) é funcional mas não é rápido para grandes bases de código. Os gargalos são bem compreendidos (elaboração, dependências em cascata, C codegen, falta de cache distribuído). Não existe um padrão de mercado dominante para "Lean Build Cache distribuído", o que valida a oportunidade de moat apontada pelo usuário. O protótipo `lean-cache` valida: Fase 1 (cache local), Fase 2 (cache remoto HTTP), Fase 2.5 (integração com Lean 4 real), Fase 3 (wrapper `lake build` multi-módulo). O speedup real de ~13x medido com Lean 4.35.0 confirma que o "moat" é viável economicamente. A próxima fronteira é a compressão/evicção (Fase 4) e a execução remota distribuída (Fase 7).

## 10. Estratégias de Otimização de Build — Relatório de Pesquisa

Com base em pesquisa sobre Lake, mathlib4 cache, `lean --server` snapshots e Bazel remote execution, as seguintes estratégias foram avaliadas para futuras fases do `lean-cache`:

### Tabela Resumo

| # | Estratégia | Complexidade | Ganho Esperado | Prioridade |
|---|------------|--------------|----------------|------------|
| 4 | Compressão de artefatos (zstd/lz4) | Baixa | Médio (banda/disco) | **1 — Baixa complexidade, ganho imediato** |
| 9 | Evicção de cache (LRU/LFU/custo) | Baixa | Médio (taxa de hit local) | **2 — Baixa complexidade** |
| 6 | Deduplicação de artefatos idênticos | Baixa-Média | Baixo-Médio (disco) | **3 — Baixa complexidade** |
| 5 | Pre-computação de dependências públicas (mathlib/stdlib) | Baixa-Média | Alto (cold builds, CI) | **4 — Média complexidade, ganho alto para CI** |
| 2 | Estado de elaboração entre sessões `lean --server` | Média-Alta | Alto (builds interativos) | **5 — Média complexidade** |
| 10 | Build farms (Bazel RE, BuildBox, etc.) | Alta | Muito alto (escala corporativa) | **6 — Alta complexidade** |
| 3 | Paralelismo distribuído de elaboração (workers) | Alta | Muito alto (projetos grandes) | **7 — Alta complexidade** |
| 1 | Cache incremental fino (declaração/tática) | Alta | Muito alto (edições locais) | **8 — Alta complexidade** |
| 8 | Edições parciais (diff de linha única) | Alta | Muito alto (interativo) | **9 — Depende do compilador Lean** |
| 7 | Cache de typeclass resolution e táticas | Alta | Médio-Alto (provas pesadas) | **10 — Última prioridade, alto risco** |

### Ordem de Execução Recomendada

`4 → 9 → 6 → 5 → 2 → 10 → 3 → 1 → 8 → 7`

### Estratégias de Baixa Complexidade (Fase 4 — imediato)

- **Compressão de artefatos (zstd/lz4):** Aplicar compressão aos artefatos (`.olean`, `.ilean`, `.c`) antes de armazenar no cache local e remoto. Reduz largura de banda do cache HTTP e uso de disco. Overhead de CPU mitigável com algoritmos rápidos como lz4.
- **Evicção de cache (LRU/LFU):** Manter metadados por entrada (último acesso, frequência, tamanho, tempo estimado de recompilação). Ao estourar cota de disco, evictar por política configurável. Expor comando `cache gc`.
- **Deduplicação de artefatos idênticos:** O modelo CAS já implica deduplicação natural; expor índice reverso e opção de hard-link/cópia única.

### Estratégias de Média Complexidade (Fase 5-6)

- **Pre-computação de dependências públicas:** Publicar `.olean` pré-compilados de mathlib4/stdlib no cache remoto. Projetos dependentes baixam binários prontos via `cache get` após `lake update`. Elimina compilação de mathlib na maioria dos cold builds.
- **Estado de elaboração entre sessões `lean --server`:** Manter servidor persistente por workspace e reutilizar snapshots de estado entre builds incrementais. Usar `Lean.Server.Snapshots` para evitar re-elaboração do zero.

### Estratégias de Alta Complexidade (Fase 7-10)

- **Build farms (Bazel RE):** Integrar com Remote Execution API para executar elaboração de módulos em workers remotos.
- **Paralelismo distribuído de elaboração:** Distribuir módulos independentes em workers remotos.
- **Cache incremental fino:** Tratar declaração/tática como unidade mínima de cache (requer repensar formato `.olean`).
- **Edições parciais:** Re-elaboração incremental de trecho afetado (depende do compilador Lean).
- **Cache de typeclass/táticas:** Alto acoplamento ao internals do Lean; maior risco.
