# Contribuindo para o lean-cache

## Build e testes

```bash
cargo build
cargo test
cargo clippy
cargo fmt --check
```

O CI usa `RUSTFLAGS=-D warnings`; o código deve compilar sem warnings.

## Padrões

- Rust std-only (sem dependências externas além das listadas em `Cargo.toml`).
- Funções públicas devem ter doc comments.
- Novos comandos CLI devem ser registrados em `print_usage()` e no `match` de `main()`.
- Testes unitários para novos módulos em `#[cfg(test)]`.

## Pull requests

- Branch a partir de `main`.
- Descreva a mudança e inclua resultados de `cargo test`.
- Atualize `README.md` e `lean-cache-PENDENCIAS.md` quando aplicável.
