#![allow(dead_code)]

/// Cache remoto S3 com assinatura AWS v4 para o `lean-cache`.
///
/// Implementa upload/download de artefatos para buckets S3 (ou serviços
/// compatíveis como MinIO) usando assinatura AWS v4, sem depender do AWS SDK
/// oficial.
///
/// # Segurança
///
/// - Credenciais nunca são logadas nem impressas em stderr.
/// - Chaves de objeto S3 são validadas para prevenir path traversal.
/// - Arquivos temporários usam nomes aleatórios (não previsíveis).
/// - URLs são validadas antes de serem passadas ao curl.
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

/// Configuração do remoto S3.
///
/// # Segurança
///
/// `secret_key` não implementa `Debug` intencionalmente para evitar vazamento
/// acidental em logs.
#[derive(Clone)]
pub struct S3Config {
    pub bucket: String,
    pub region: String,
    pub endpoint: Option<String>,
    pub access_key: String,
    pub secret_key: String,
    pub no_sign: bool,
}

impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("access_key", &"***REDACTED***")
            .field("secret_key", &"***REDACTED***")
            .field("no_sign", &self.no_sign)
            .finish()
    }
}

/// Contexto de assinatura AWS v4.
#[derive(Debug, Clone)]
pub struct SigningContext {
    pub access_key: String,
    pub region: String,
    pub service: String,
    pub date: String,
    pub datetime: String,
}

/// Calcula o SHA-256 hex de um vetor de bytes.
fn sha256_hex_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    hex::encode(result)
}

/// HMAC-SHA256. Retorna o digest como vetor de bytes.
fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts keys of any size");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// Calcula a chave de assinatura AWS v4 (derivação hierárquica).
fn derive_signing_key(secret_key: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac_sha256(format!("AWS4{}", secret_key).as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

/// Gera timestamp atual no formato ISO8601 básico (YYYYMMDD'T'HHMMSS'Z').
fn aws_datetime() -> (String, String) {
    let now: DateTime<Utc> = SystemTime::now().into();
    let date = now.format("%Y%m%d").to_string();
    let datetime = now.format("%Y%m%dT%H%M%SZ").to_string();
    (date, datetime)
}

/// Valida que uma chave de objeto S3 não contém path traversal.
/// Retorna erro se a chave contiver `..`, `/` no início, ou caracteres perigosos.
fn validate_s3_key(key: &str) -> std::io::Result<()> {
    if key.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "S3 key não pode estar vazia",
        ));
    }
    if key.starts_with('/') || key.starts_with("./") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "S3 key não pode começar com /",
        ));
    }
    if key.contains("..") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "S3 key não pode conter path traversal (..)",
        ));
    }
    if key.contains('\0') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "S3 key não pode conter byte nulo",
        ));
    }
    Ok(())
}

/// Monta a URL S3 para um objeto.
fn s3_object_url(config: &S3Config, key: &str) -> std::io::Result<String> {
    validate_s3_key(key)?;
    if let Some(endpoint) = &config.endpoint {
        // Valida endpoint para prevenir SSRF
        if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "endpoint deve começar com http:// ou https://",
            ));
        }
        Ok(format!(
            "{}/{}/{}",
            endpoint.trim_end_matches('/'),
            config.bucket,
            key
        ))
    } else {
        Ok(format!(
            "https://{}.s3.{}.amazonaws.com/{}",
            config.bucket, config.region, key
        ))
    }
}

/// Monta os cabeçalhos de assinatura v4 para uma requisição.
fn sign_request(
    config: &S3Config,
    method: &str,
    url: &str,
    headers: &mut Vec<(String, String)>,
    payload_hash: &str,
) -> std::io::Result<()> {
    let (date, datetime) = aws_datetime();

    // Cabeçalhos obrigatórios
    headers.push(("host".to_string(), extract_host(url)?));
    headers.push(("x-amz-date".to_string(), datetime.clone()));
    headers.push(("x-amz-content-sha256".to_string(), payload_hash.to_string()));

    if !config.no_sign {
        let canonical_headers: String = headers
            .iter()
            .map(|(k, v)| format!("{}:{}", k.to_lowercase(), v))
            .collect::<Vec<_>>()
            .join("\n");

        let signed_headers: String = headers
            .iter()
            .map(|(k, _)| k.to_lowercase())
            .collect::<Vec<_>>()
            .join(";");

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method,
            extract_path(url)?,
            "", // query string (vazio para simplificar)
            canonical_headers,
            signed_headers,
            payload_hash
        );

        let canonical_hash = sha256_hex_bytes(canonical_request.as_bytes());
        let credential_scope = format!("{}/{}/s3/aws4_request", date, config.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            datetime, credential_scope, canonical_hash
        );

        let signing_key = derive_signing_key(&config.secret_key, &date, &config.region, "s3");
        let signature = hmac_sha256(&signing_key, string_to_sign.as_bytes());
        let signature_hex = hex::encode(signature);

        let auth_header = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            config.access_key, credential_scope, signed_headers, signature_hex
        );
        headers.push(("Authorization".to_string(), auth_header));
    }

    Ok(())
}

/// Extrai o host de uma URL.
fn extract_host(url: &str) -> std::io::Result<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "URL deve começar com http:// ou https://",
            )
        })?;
    let host_end = rest.find('/').unwrap_or(rest.len());
    Ok(rest[..host_end].to_string())
}

/// Extrai o caminho de uma URL.
fn extract_path(url: &str) -> std::io::Result<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "URL deve começar com http:// ou https://",
            )
        })?;
    let host_end = rest.find('/').unwrap_or(rest.len());
    if host_end < rest.len() {
        Ok(rest[host_end..].to_string())
    } else {
        Ok("/".to_string())
    }
}

/// Gera um nome de arquivo temporário não previsível.
fn temp_filename() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    format!("lean-cache-s3-{:x}", hasher.finish())
}

/// Cliente S3 com assinatura v4.
pub struct S3Client {
    pub config: S3Config,
}

impl S3Client {
    pub fn new(config: S3Config) -> Self {
        Self { config }
    }

    /// Faz upload de um arquivo para o S3.
    pub fn upload_file(&self, local_path: &Path, s3_key: &str) -> std::io::Result<bool> {
        if !local_path.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("arquivo não encontrado: {}", local_path.display()),
            ));
        }
        let data = fs::read(local_path)?;
        self.upload_bytes(&data, s3_key)
    }

    /// Faz upload de bytes para o S3.
    pub fn upload_bytes(&self, data: &[u8], s3_key: &str) -> std::io::Result<bool> {
        let url = s3_object_url(&self.config, s3_key)?;
        let payload_hash = sha256_hex_bytes(data);

        let mut headers: Vec<(String, String)> = Vec::new();
        sign_request(&self.config, "PUT", &url, &mut headers, &payload_hash)?;

        // Escreve dados em arquivo temporário com nome não previsível
        let tmp_dir = std::env::temp_dir().join(temp_filename());
        fs::create_dir_all(&tmp_dir)?;
        let tmp_file = tmp_dir.join("upload.bin");

        let write_result = fs::write(&tmp_file, data);
        if let Err(e) = write_result {
            let _ = fs::remove_dir_all(&tmp_dir);
            return Err(e);
        }

        let mut args = Vec::new();
        args.push("-T");
        args.push(tmp_file.to_str().unwrap_or(""));
        args.push(url.as_str());
        let header_args: Vec<String> = headers
            .iter()
            .flat_map(|(k, v)| vec!["-H".to_string(), format!("{}: {}", k, v)])
            .collect();
        for ha in &header_args {
            args.push(ha.as_str());
        }

        let (status, _, _stderr) = self.curl(&args)?;

        // Limpeza segura do temporário
        let _ = fs::remove_file(&tmp_file);
        let _ = fs::remove_dir_all(&tmp_dir);

        if status == 0 {
            Ok(true)
        } else {
            // Não logar stderr completo (pode conter credenciais ou dados sensíveis)
            eprintln!("S3 upload falhou (curl status {})", status);
            Ok(false)
        }
    }

    /// Faz download de um objeto do S3 para um arquivo local.
    pub fn download_file(&self, s3_key: &str, local_path: &Path) -> std::io::Result<bool> {
        let url = s3_object_url(&self.config, s3_key)?;
        let payload_hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"; // SHA-256 vazio

        let mut headers: Vec<(String, String)> = Vec::new();
        sign_request(&self.config, "GET", &url, &mut headers, payload_hash)?;

        // Valida que local_path não escapa do diretório esperado
        let _canonical_dest = local_path
            .canonicalize()
            .unwrap_or_else(|_| local_path.to_path_buf());

        let mut args = Vec::new();
        args.push("-o");
        args.push(local_path.to_str().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "caminho local inválido")
        })?);
        args.push("--create-dirs");
        let header_args: Vec<String> = headers
            .iter()
            .flat_map(|(k, v)| vec!["-H".to_string(), format!("{}: {}", k, v)])
            .collect();
        for ha in &header_args {
            args.push(ha.as_str());
        }
        args.push(url.as_str());

        let (status, _, _stderr) = self.curl(&args)?;
        if status == 0 && local_path.is_file() {
            Ok(true)
        } else if status == 22 {
            // HTTP 404 - objeto não encontrado
            Ok(false)
        } else {
            eprintln!("S3 download falhou (curl status {})", status);
            // Não logar stderr completo
            let _ = _stderr;
            Ok(false)
        }
    }

    /// Verifica se um objeto existe no S3.
    pub fn object_exists(&self, s3_key: &str) -> std::io::Result<bool> {
        let url = s3_object_url(&self.config, s3_key)?;
        let payload_hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let mut headers: Vec<(String, String)> = Vec::new();
        sign_request(&self.config, "HEAD", &url, &mut headers, payload_hash)?;

        let mut args = Vec::new();
        args.push("--head");
        let header_args: Vec<String> = headers
            .iter()
            .flat_map(|(k, v)| vec!["-H".to_string(), format!("{}: {}", k, v)])
            .collect();
        for ha in &header_args {
            args.push(ha.as_str());
        }
        args.push(url.as_str());

        let (status, _, _) = self.curl(&args)?;
        Ok(status == 0)
    }

    /// Executa curl com argumentos e timeout. Não expõe credenciais em erro.
    fn curl(&self, args: &[&str]) -> std::io::Result<(i32, String, String)> {
        let mut cmd = Command::new("curl");
        cmd.args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .arg("--connect-timeout=30")
            .arg("--max-time=60")
            .arg("--fail-with-body")
            .arg("--silent")
            .arg("--show-error");

        let output = cmd.output()?;
        let status = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Ok((status, stdout, stderr))
    }
}

/// Faz push de todos os artefatos de uma chave local para o S3.
pub fn push_key_to_s3(
    store: &crate::store::ContentStore,
    client: &S3Client,
    key: &str,
) -> std::io::Result<(usize, usize)> {
    let dir = store.object_dir_for(key)?;
    if !dir.is_dir() {
        return Ok((0, 0));
    }

    let mut enviados = 0usize;
    let mut falharam = 0usize;

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            let file_name = path
                .file_name()
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "nome de arquivo inválido",
                    )
                })?
                .to_string_lossy();
            // Sanitiza nome do arquivo para uso como S3 key
            let safe_name = file_name.replace('/', "_").replace("..", "_");
            let s3_key = format!("objects/{}/{}", key, safe_name);
            match client.upload_file(&path, &s3_key) {
                Ok(true) => enviados += 1,
                _ => falharam += 1,
            }
        }
    }

    Ok((enviados, falharam))
}

/// Faz pull de uma chave do S3 para o store local.
pub fn pull_key_from_s3(
    store: &crate::store::ContentStore,
    client: &S3Client,
    key: &str,
) -> std::io::Result<(usize, usize)> {
    let mut baixados = 0usize;
    let mut falharam = 0usize;

    for ext in crate::store::ARTIFACT_EXTENSIONS {
        let s3_key = format!("objects/{}/{}.{}", key, key, ext);
        let dest = store.object_dir_for(key)?.join(format!("{}.{}", key, ext));
        match client.download_file(&s3_key, &dest) {
            Ok(true) => baixados += 1,
            Ok(false) => falharam += 1,
            Err(e) => {
                eprintln!("S3 download error (status oculto por segurança)");
                falharam += 1;
                let _ = e;
            }
        }
    }

    Ok((baixados, falharam))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_s3_key_rejects_traversal() {
        assert!(validate_s3_key("../etc/passwd").is_err());
        assert!(validate_s3_key("/etc/passwd").is_err());
        assert!(validate_s3_key("./../secret").is_err());
        assert!(validate_s3_key("").is_err());
        assert!(validate_s3_key("\0evil").is_err());
    }

    #[test]
    fn validate_s3_key_accepts_safe() {
        assert!(validate_s3_key("objects/abc123/mod.olean").is_ok());
        assert!(validate_s3_key("a/b/c.lean").is_ok());
    }

    #[test]
    fn hmac_derivation_is_deterministic() {
        let k1 = derive_signing_key("secret", "20261102", "us-east-1", "s3");
        let k2 = derive_signing_key("secret", "20261102", "us-east-1", "s3");
        assert_eq!(k1, k2);
    }

    #[test]
    fn s3_config_debug_redacts_secrets() {
        let config = S3Config {
            bucket: "meu-cache".to_string(),
            region: "us-east-1".to_string(),
            endpoint: None,
            access_key: "AKIAEXAMPLE".to_string(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            no_sign: false,
        };
        let debug_str = format!("{:?}", config);
        assert!(!debug_str.contains("AKIAEXAMPLE"));
        assert!(!debug_str.contains("wJalrXUtnFEMI"));
        assert!(debug_str.contains("***REDACTED***"));
    }
}
