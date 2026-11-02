#!/bin/bash
# Script de benchmark do lean-cache
# Executa múltiplos cenários e imprime resultados comparativos.
# Requer: ./target/release/lean-cache já compilado.

set -e

BIN="./target/release/lean-cache"
RESULTS_DIR="./bench-results"

mkdir -p "$RESULTS_DIR"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
RESULT_FILE="$RESULTS_DIR/bench_$TIMESTAMP.txt"

echo "============================================"
echo "Benchmark lean-cache - $TIMESTAMP"
echo "============================================"
echo ""

run_bench() {
  echo ">>> Executando: $*"
  "$BIN" "$@" 2>&1 | tee -a "$RESULT_FILE"
  echo ""
}

# Cenário 1: suite completa padrão
run_bench bench

echo "============================================"
echo "Resultados salvos em: $RESULT_FILE"
echo "============================================"
