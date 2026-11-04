# Como uma LLM lê e compreende arquivos de código

Este documento explica o mecanismo de leitura de arquivos por um modelo de linguagem (LLM) como eu, e como isso afeta a edição de código em projetos grandes.

---

## 1. O modelo de leitura: janela de contexto por vez

Uma LLM **não carrega o arquivo inteiro na memória de uma só vez** como um editor de texto faria com "Ctrl+A". Em vez disso:

- Cada chamada de leitura (`read`) traz um **bloco limitado de linhas** (máximo ~2.000 linhas por chamada).
- O modelo processa esse bloco como um contexto temporário.
- Após a resposta, esse contexto pode ser descartado ou mantido parcialmente, dependendo da sessão.

### Implicação prática

Se um arquivo tem 1.570 linhas (como `main.rs`), eu consigo lê-lo em 1 ou 2 chamadas. Se um arquivo tem 10.000 linhas, preciso de várias chamadas e posso **esquecer detalhes do início** ao chegar ao final.

---

## 2. Como o `edit` funciona (find-and-replace exato)

A ferramenta `edit` não entende "intenção". Ela faz uma **busca de texto literal** (string matching):

```text
oldString: "Erro: arquivo nao encontrado: {}"
newString: "Error: file not found: {}"
```

Requisitos para funcionar:

| Requisito | Por que importa |
|-----------|-----------------|
| `oldString` deve ser **único** no arquivo | Se aparecer 2 vezes, o edit falha com "multiple matches" |
| Espaços, tabs e quebras de linha devem **bater exatamente** | Uma diferença de espaço impede a substituição |
| Caracteres especiais (acentos, aspas) devem ser **idênticos** | `não` != `nao` (sem acento) |

### Problema comum com arquivos grandes

Quando tentei traduzir `print_usage()`, o `edit` falhou porque o `oldString` não batia exatamente com as sequências de escape (`\n\`) presentes no arquivo. Isso acontece porque o arquivo-fonte tem barras invertidas literais que são difíceis de reproduzir no prompt.

---

## 3. Estratégias para acelerar o trabalho com arquivos grandes

### 3.1. Escrever o arquivo completo de uma vez (`write`)

Para arquivos de até ~3.000 linhas, o `write` é mais eficiente que dezenas de `edit`:

- Vantagem: substituo o conteúdo inteiro de uma vez, sem risco de `oldString` não bater.
- Desvantagem: preciso ter o conteúdo completo pronto antes de enviar.

### 3.2. Editar em blocos pequenos e independentes

Dividir o arquivo em seções lógicas (função por função) e editar cada uma separadamente:

```
1. print_usage()       <- traduzido
2. cmd_init()          <- pendente
3. cmd_hash()          <- pendente
...
```

Isso reduz o tamanho do `oldString` e diminui a chance de erro de correspondência.

### 3.3. Usar `replaceAll: true` para renomeações em massa

Se preciso mudar `PathBuf::from(".lean-cache")` para `store_dir()` em 16 lugares, uso `replaceAll: true` em vez de 16 edições individuais.

---

## 4. Limites atuais nesta sessão

| Limite | Detalhe |
|--------|---------|
| Linhas por leitura | ~2.000 |
| Tamanho máximo de resposta | ~4.000 tokens (output) |
| Arquivos lidos nesta sessão | `main.rs` (1.570 linhas), `store.rs`, `remote.rs`, etc. |

### Por que a tradução de `main.rs` está demorando

O arquivo `main.rs` tem 1.570 linhas, mas cerca de 80% são **strings visíveis ao usuário** (mensagens de erro, `println!`, `eprintln!`). Traduzir linha por linha com `edit` exigiria ~50-100 chamadas individuais, cada uma com risco de falha por correspondência exata.

---

## 5. Recomendação para acelerar

Para terminar a tradução de `main.rs` para inglês rapidamente:

1. **Use `write` para reescrever o arquivo completo** em inglês de uma vez. Como o arquivo tem 1.570 linhas, cabe em uma única resposta se for bem estruturado.
2. **Não traduza comentários** (como você sugeriu) — isso reduz ~30% do trabalho.
3. **Mantenha a lógica de código idêntica** — apenas mude strings entre aspas (`"..."`) e mensagens de `println!` / `eprintln!`.
4. **Depois, compile** com `cargo build --release` para verificar erros.

---

## 6. Exemplo do que funciona vs. o que não funciona

### ❌ Não funciona (falha por correspondência exata)

```
oldString: "Uso: lean-cache <comando> [argumentos]"
newString: "Usage: lean-cache <command> [arguments]"
```

*Motivo: o arquivo tem `\n\` (barras invertidas literais) entre cada linha do `println!`. O `oldString` acima não inclui essas barras, então não bate.*

### ✅ Funciona (reescrever o bloco inteiro)

```
oldString: fn print_usage() { ... toda a função ... }
newString: fn print_usage() { ... versão em inglês ... }
```

---

## 7. Próximos passos sugeridos

1. Reescrever `main.rs` completo em inglês com `write` (arquivo ~1.500 linhas).
2. Corrigir testes em `remote.rs` e `store.rs` que usam "exemplo.com".
3. Compilar com `cargo build --release`.
4. Testar comandos no `lean-test-proj`.
5. Fazer commit e PR para o GitHub.

---

*Documento criado para referência da sessão. Última atualização: 04/Nov/2026.*
