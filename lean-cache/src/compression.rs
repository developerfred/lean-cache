#![allow(dead_code)]

/// Módulo de compressão de artefatos para reduzir uso de disco e banda de rede.
/// Suporta múltiplos algoritmos com fallback automático:
///  - zstd (melhor taxa de compressão, requer biblioteca C)
///  - lz4 (velocidade extrema, pure Rust via lz4_flex)
///  - gzip (fallback via comando externo, std-only)

use crate::sha256::sha256_hex;
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

/// Algoritmo de compressão disponível.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionAlgorithm {
    Zstd,
    Lz4,
    Gzip,
    None,
}

impl CompressionAlgorithm {
    pub fn from_str(s: &str) -> Self {
        match s {
            "zstd" => CompressionAlgorithm::Zstd,
            "lz4" => CompressionAlgorithm::Lz4,
            "gzip" => CompressionAlgorithm::Gzip,
            _ => CompressionAlgorithm::None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            CompressionAlgorithm::Zstd => ".zst",
            CompressionAlgorithm::Lz4 => ".lz4",
            CompressionAlgorithm::Gzip => ".gz",
            CompressionAlgorithm::None => "",
        }
    }

    /// Verifica se este algoritmo está disponível no sistema atual.
    pub fn is_available(self) -> bool {
        match self {
            CompressionAlgorithm::Zstd => true, // zstd crate está compilada
            CompressionAlgorithm::Lz4 => true,  // lz4_flex crate está compilada
            CompressionAlgorithm::Gzip => gzip_available(),
            CompressionAlgorithm::None => true,
        }
    }

    /// Escolhe o melhor algoritmo disponível com base na preferência.
    pub fn best_available(preferred: CompressionAlgorithm) -> Self {
        if preferred != CompressionAlgorithm::None && preferred.is_available() {
            return preferred;
        }
        // Ordem de preferência: zstd > lz4 > gzip > none
        for alg in [
            CompressionAlgorithm::Zstd,
            CompressionAlgorithm::Lz4,
            CompressionAlgorithm::Gzip,
        ] {
            if alg.is_available() {
                return alg;
            }
        }
        CompressionAlgorithm::None
    }
}

/// Comprime dados em memória usando o algoritmo especificado.
pub fn compress(data: &[u8], algorithm: CompressionAlgorithm) -> io::Result<Vec<u8>> {
    match algorithm {
        CompressionAlgorithm::Zstd => {
            let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3)?;
            encoder.write_all(data)?;
            encoder.finish()
        }
        CompressionAlgorithm::Lz4 => {
            let mut compressed = Vec::with_capacity(data.len());
            let _ = lz4_flex::block::compress_into(data, &mut compressed);
            Ok(compressed)
        }
        CompressionAlgorithm::Gzip => compress_gzip_bytes(data),
        CompressionAlgorithm::None => Ok(data.to_vec()),
    }
}

/// Descomprime dados em memória usando o algoritmo especificado.
pub fn decompress(data: &[u8], algorithm: CompressionAlgorithm) -> io::Result<Vec<u8>> {
    match algorithm {
        CompressionAlgorithm::Zstd => {
            let mut decoder = zstd::stream::read::Decoder::new(data)?;
            let mut decompressed = Vec::new();
            decoder.read_to_end(&mut decompressed)?;
            Ok(decompressed)
        }
        CompressionAlgorithm::Lz4 => {
            // Precisamos saber o tamanho original; lz4_flex block decompress requer tamanho.
            // Como não armazenamos o tamanho, tentamos com um buffer grande.
            let mut decompressed = vec![0u8; data.len() * 10];
            let actual = lz4_flex::block::decompress_into(data, &mut decompressed)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("lz4 decompress error: {}", e)))?;
            decompressed.truncate(actual);
            Ok(decompressed)
        }
        CompressionAlgorithm::Gzip => decompress_gzip_bytes(data),
        CompressionAlgorithm::None => Ok(data.to_vec()),
    }
}

/// Comprime um arquivo de origem para um arquivo de destino.
pub fn compress_file(src: &Path, dst: &Path, algorithm: CompressionAlgorithm) -> io::Result<()> {
    let data = fs::read(src)?;
    let compressed = compress(&data, algorithm)?;
    fs::write(dst, compressed)?;
    Ok(())
}

/// Descomprime um arquivo de origem para um arquivo de destino.
pub fn decompress_file(src: &Path, dst: &Path, algorithm: CompressionAlgorithm) -> io::Result<()> {
    let data = fs::read(src)?;
    let decompressed = decompress(&data, algorithm)?;
    fs::write(dst, decompressed)?;
    Ok(())
}

/// Calcula o hash SHA-256 dos dados comprimidos para verificação de integridade.
pub fn compressed_hash(data: &[u8], algorithm: CompressionAlgorithm) -> io::Result<String> {
    let compressed = compress(data, algorithm)?;
    Ok(sha256_hex(&compressed))
}

// ---------------------------------------------------------------------------
// Fallback gzip via comando externo (mantido para std-only e compatibilidade)
// ---------------------------------------------------------------------------

/// Verifica se o gzip está disponível no sistema.
fn gzip_available() -> bool {
    Command::new("gzip")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Comprime bytes usando gzip via comando externo.
fn compress_gzip_bytes(data: &[u8]) -> io::Result<Vec<u8>> {
    let mut child = Command::new("gzip")
        .arg("-c")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| io::Error::new(io::ErrorKind::NotFound, format!("gzip não encontrado: {}", e)))?;

    {
        let stdin = child.stdin.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::Other, "não foi possível abrir stdin do gzip")
        })?;
        stdin.write_all(data)?;
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "gzip falhou ao comprimir",
        ));
    }
    Ok(output.stdout)
}

/// Descomprime bytes usando gzip via comando externo.
fn decompress_gzip_bytes(data: &[u8]) -> io::Result<Vec<u8>> {
    let mut child = Command::new("gzip")
        .arg("-d")
        .arg("-c")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| io::Error::new(io::ErrorKind::NotFound, format!("gzip não encontrado: {}", e)))?;

    {
        let stdin = child.stdin.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::Other, "não foi possível abrir stdin do gzip")
        })?;
        stdin.write_all(data)?;
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "gzip falhou ao descomprimir",
        ));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zstd_roundtrip() {
        let data = b"conteudo de teste para compressao zstd do lean-cache";
        let compressed = compress(data, CompressionAlgorithm::Zstd).unwrap();
        assert!(compressed.len() < data.len());
        let decompressed = decompress(&compressed, CompressionAlgorithm::Zstd).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn lz4_roundtrip() {
        let data = b"conteudo de teste para compressao lz4 do lean-cache";
        let compressed = compress(data, CompressionAlgorithm::Lz4).unwrap();
        let decompressed = decompress(&compressed, CompressionAlgorithm::Lz4).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn none_roundtrip() {
        let data = b"conteudo sem compressao";
        let compressed = compress(data, CompressionAlgorithm::None).unwrap();
        assert_eq!(compressed, data);
        let decompressed = decompress(&compressed, CompressionAlgorithm::None).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn best_available_returns_something() {
        let alg = CompressionAlgorithm::best_available(CompressionAlgorithm::Zstd);
        assert!(alg.is_available());
    }
}
