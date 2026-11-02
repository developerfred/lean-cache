# Contribuindo com o lean-cache

Obrigado por se interessar em contribuir com o **lean-cache**! Este projeto acelera builds do Lean 4 via cache endereçável por conteúdo. Todas as contribuições são bem-vindas.

## Como contribuir

### 1. Reporte bugs
Abra uma issue no GitHub descrevendo:
- Versão do Lean 4 (`lean --version`)
- Sistema operacional e arquitetura
- Passos para reproduzir
- Comportamento esperado vs observado

### 2. Sugira features
Abra uma issue com a tag `enhancement` descrevendo:
- O problema que você quer resolver
- Sua solução proposta
- Exemplos de uso (se aplicável)

### 3. Envie Pull Requests

1. **Faça um fork** do repositório
2. **Crie uma branch** a partir de `main`:
   ```bash
   git checkout -b feature/minha-feature
   ```
3. **Faça suas alterações** e teste:
   ```bash
   cargo build --release
   cargo test
   ```
4. **Commit** com mensagens claras:
   ```bash
   git commit -m "feat: descrição da sua alteração"
   ```
5. **Push** para sua branch:
   ```bash
   git push origin feature/minha-feature
   ```
6. **Abra um Pull Request** no GitHub

### Requisitos de código

- **Rust edition 2021**
- **Std-only**: evite adicionar dependências externas (crates.io). Se precisar, justifique no PR.
- **CLI em português**: mensagens de usuário em PT-BR.
- **Comentários em português** no código.
- **Testes**: adicione testes unitários para nova funcionalidade.

### Estrutura do projeto

```
lean-cache/
├── src/
│   ├── main.rs      # CLI e comando build
│   ├── sha256.rs    # SHA-256 puro (sem dependências)
│   ├── store.rs     # Cache endereçável por conteúdo
│   ├── remote.rs    # Cache remoto HTTP (via curl)
│   └── bench.rs     # Suite de benchmark
├── Cargo.toml
├── README.md
└── bench.sh
```

### Regras de revisão de PR

- Todos os testes devem passar (`cargo test`)
- O código deve compilar em modo release (`cargo build --release`)
- Novas funcionalidades devem ter testes
- Documentação atualizada (README, PESQUISA_LEAN_BUILD.md se aplicável)

### Código de conduta

Seja respeitoso com outros contribuidores. Este é um projeto comunitário open source.

## Licença

Este projeto está sob licença MIT. Veja o arquivo `LICENSE`.

## Contatos

- Issues: https://github.com/developerfred/lean-cache/issues
- Email: codingsh@pm.me
