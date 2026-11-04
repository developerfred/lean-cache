# Arquitetura do lean-cache

## Visão geral

`lean-cache` é um wrapper em Rust (std-only, offline) ao `lake build` que
implementa cache endereçável por conteúdo para artefatos Lean 4 (`.olean`,
`.ilean`, `.c`, `.o`).

## Fluxo de `lean-cache build <raiz.lean>`

1. `resolve_import_graph` resolve imports em ordem topológica.
2. Para cada módulo:
   - lê fonte;
   - calcula chave = SHA-256(fonte + chaves dos imports + versão Lean + flags);
   - extrai declarações cirúrgicas e calcula `decl_key` para cada uma;
   - verifica `ContentStore::contains(key)` e `SurgicalStore::contains(decl_key)`.
3. Se todos os módulos estão em hit:
   - restaura artefatos para `.lake/build/lib/lean/...`;
   - pula o `lake build`.
4. Caso contrário:
   - tenta cache remoto (HTTP/S3) para módulos em miss;
   - executa `lake build`;
   - armazena artefatos no store local e (opcional) remoto.

## Módulos

- `store.rs`: ContentStore, TraceRecord, validação de chave, LRU GC.
- `compression.rs`: zstd/lz4/gzip condicional (≥ 64 KB).
- `remote.rs`: cliente HTTP via curl, retry/backoff, push/pull/sync, manifesto.
- `s3.rs`: assinatura AWS v4, upload/download.
- `worker.rs`: pool round-robin, RemoteWorkerClient, base64.
- `snapshots.rs`: SnapshotStore + LeanServer (JSON-RPC).
- `surgical.rs`: extração de declarações e SurgicalStore.
- `bench.rs`: benchmark simulado.
- `sha256.rs`: SHA-256 puro.

## Formato do store

```
.lean-cache/
  remote.txt
  workers.txt
  config.txt
  objects/<prefix>/<key>/...
  snapshots/<prefix>/<key>.json
  surgical/<prefix>/<key>/decl.lean
```

## Segurança

Validação de chave, path traversal, URLs, credenciais redacted. Ver README.
