# lean-cache — Content-addressable Build Cache for Lean 4

**Acelere builds do Lean 4 em até 40× com cache endereçável por conteúdo.**

O `lean-cache` é um wrapper CLI escrito em Rust (std-only, zero dependências externas) que funciona como um cache de artefatos para o sistema de build `lake` do Lean 4. Ele resolve o grafo de `import`s, calcula chaves de conteúdo por módulo e, quando todos os módulos estão em cache hit, pula a invocação do `lake build` inteiro restaurando os `.olean`, `.ilean`, `.c` e `.o` diretamente.

---

## 🚀 Resultados Reais

| Projeto | Módulos | Build Frio | Build Quente | Speedup |
|---------|---------|------------|--------------|---------|
| lean-test-proj | 6 | ~11.5s | ~0.29s | **~40×** |
| droidsaw-lean | 8 | ~164s | ~6.9s | **~24×** |

---

## 📦 Instalação

```bash
# Clone o repositório
git clone https://github.com/developerfred/lean-cache.git
cd lean-cache/lean-cache

# Compile em release
cargo build --release

# Instale
cp target/release/lean-cache /usr/local/bin/
```

> Requer: Rust toolchain, `curl` e (opcionalmente) `gzip` / `zstd`.

---

## 🛠️ Uso Rápido

```bash
# Dentro de um projeto Lake (tem lakefile.toml ou lakefile.lean)
lean-cache init
lean-cache build MeuArquivo.lean
```

| Comando | Descrição |
|---------|-----------|
| `init` | Inicializa o store de cache (`.lean-cache`) |
| `hash <arquivo.lean>` | Calcula a chave de conteúdo de um módulo |
| `store <chave> <dir>` | Armazena artefatos de um diretório sob uma chave |
| `get <chave> <dir>` | Restaura artefatos de uma chave para um diretório |
| `build <arquivo.raiz.lean>` | Build acelerado: usa cache ou executa `lake build` |
| `status` | Estatísticas do store local |
| `clean` | Remove o store de cache local |
| `gc` | Garbage collection LRU (remove entradas menos acessadas) |
| `remote-set <url>` | Define URL base do cache remoto HTTP |
| `push / pull / sync <chave>` | Envia/baixa/sincroniza artefatos com o remoto |
| `s3-push / s3-pull / s3-sync <chave>` | Idem para AWS S3 (assinatura v4) |
| `snap / snap-load / snap-list` | Snapshots de elaboração `lean --server` |
| `surgical-list / surgical-store / surgical-get <arquivo.lean>` | Edição cirúrgica por declaração |
| `bench` | Suite de benchmark simulado |

---

## 🏗️ Arquitetura

```
.lean-cache/
  remote.txt          # config do cache remoto HTTP
  objects/
    ab/
    abcd1234/         # .olean .ilean .c .o (opcionalmente .zst/.lz4/.gz)
  snapshots/
    ab/
    abcd1234.json     # snapshot de elaboração JSON
  surgical/
    ab/
    abcd1234/         # declarações individuais endereçáveis por conteúdo
```

**Modelo de endereçamento:**
```
chave = SHA-256( fonte + chaves_dos_imports + versão_Lean + flags )
caminho = .lean-cache/objects/<chave[0:2]>/<chave>/
```

---

## 🔒 Segurança

- Validação de chave de cache (rejeita path traversal `..`, caminhos absolutos, byte nulo)
- Validação de chave S3 e URL (SSRF protection)
- Validação de nome de módulo Lean
- Arquivos temporários com nomes não previsíveis (hash de tempo + PID)
- Credenciais S3 redacted em logs (`Debug` oculta `access_key`/`secret_key`)
- Erros de rede mostram apenas status code, nunca URLs ou credenciais

---

## 📊 Roadmap

| Fase | Entregável | Status |
|------|------------|--------|
| 1 | Cache local CAS + SHA-256 puro | ✅ |
| 2 | Cache remoto HTTP + retry/backoff | ✅ |
| 2.5 | Integração com Lean 4 real | ✅ |
| 3 | Wrapper `lake build` multi-módulo | ✅ |
| 4 | Compressão zstd/lz4 + evicção LRU + workers + S3 v4 + snapshots + cirúrgico | ✅ |
| 5 | Build Advisor (direção WSP) | 🔄 Planejado |

---

## 📖 Documentação

- [`lean-cache/README.md`](lean-cache/README.md) — guia completo de uso
- [`NOTAS_DESAFIOS_IMPEDIMENTOS.md`](NOTAS_DESAFIOS_IMPEDIMENTOS.md) — desafios e decisões de design
- [`PESQUISA_LEAN_BUILD.md`](PESQUISA_LEAN_BUILD.md) — pesquisa sobre builds do Lean 4
- [`lean-cache/BENCHMARK_RESULTS.md`](lean-cache/BENCHMARK_RESULTS.md) — resultados de benchmark

---

## 🤝 Contribuindo

1. Faça um fork do repositório
2. Crie uma branch para sua feature (`git checkout -b feature/minha-feature`)
3. Commit suas mudanças (`git commit -m "feat: ..."`)
4. Push para a branch (`git push origin feature/minha-feature`)
5. Abra um Pull Request

---

## 📄 Licença

MIT License. Veja o arquivo [LICENSE](LICENSE) para mais detalhes.

---

**Desenvolvido por codingsh** (codingsh@pm.me · x.com/codingsh) — Acelerando builds do Lean 4, um módulo de cada vez.
