# Benchmark do lean-cache — Grandes Projetos Lean 4

Este documento registra os resultados de benchmark do `lean-cache` em projetos reais do GitHub.

## Metodologia

Cada projeto foi testado com:
1. **Build frio:** `lean-cache clean && rm -rf .lake/build && time lean-cache build Main.lean`
2. **Build quente:** `rm -rf .lake/build && time lean-cache build Main.lean`

Speedup = Tempo frio / Tempo quente

## Resultados

| # | Projeto | URL | Módulos | Build Frio | Build Quente | Speedup | Data |
|---|---------|-----|---------|-------------|--------------|---------|------|
| 1 | lean-test-proj (local) | `/Volumes/MASS/builder-the-best/lean-test-proj` | 6 | ~11.5s | ~0.29s | ~40x | 02/Nov/2026 |
| 2 | chronos-vault-security | https://github.com/Chronos-Vault/chronos-vault-security | — | — | — | — | Pendente |
| 3 | droidsaw-lean | https://github.com/droidsaw/droidsaw-lean | — | — | — | — | Pendente |
| 4 | L4YAML | https://github.com/nasa-jpl/L4YAML | — | — | — | — | Pendente |
| 5 | mathlib4 (amostra) | https://github.com/leanprover-community/mathlib4 | — | — | — | — | Pendente |

## Como Executar

```bash
# Clonar um projeto
git clone <url-do-projeto>
cd <projeto>

# Inicializar o cache
lean-cache init

# Build frio
lean-cache clean
rm -rf .lake/build
time lean-cache build <arquivo-raiz.lean>

# Build quente
rm -rf .lake/build
time lean-cache build <arquivo-raiz.lean>
```

## Observações

- O `mathlib4` é o stress test definitivo (centenas de milhares de linhas, 1.000+ módulos)
- Para projetos com dependências externas (mathlib), use `lake update` antes do build frio
- O `lean-cache` funciona como wrapper ao `lake build`, então requer `lakefile.toml` ou `lakefile.lean`
