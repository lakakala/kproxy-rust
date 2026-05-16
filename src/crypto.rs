//! 加密控制连接使用的小型加密辅助模块。
//!
//! 配置中的 token 会被哈希成固定的 AES-256 密钥。每一帧都会用
//! AES-256-GCM 和新的随机 nonce 独立加密；nonce 会放在密文前面，
//! 供接收端解密该帧。

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// 从共享 token 派生稳定的 32 字节 AES 密钥。
///
/// 这里使用简单的 SHA-256 派生，而不是密码 KDF，因此 token 应当具备高熵。
pub fn derive_key(token: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let result = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&result);
    key
}

/// 加密明文帧体，并返回 `nonce || ciphertext || tag`。
///
/// AES-GCM 使用 12 字节 nonce，并在密文后追加 16 字节认证标签。
/// 每一帧都会生成随机 nonce，以避免 nonce 重用。
pub fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
    let cipher_key = Key::<Aes256Gcm>::from_slice(key);
    let cipher = Aes256Gcm::new(cipher_key);

    let mut nonce_bytes = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("Encryption failed: {}", e))?;

    let mut result = Vec::with_capacity(12 + ciphertext.len());
    result.extend_from_slice(&nonce_bytes);
    result.extend_from_slice(&ciphertext);
    Ok(result)
}

/// 解密由 [`encrypt`] 生成的字节。
///
/// 输入必须包含 12 字节 nonce、密文以及 16 字节 GCM 标签。
/// 认证失败会作为错误返回。
pub fn decrypt(key: &[u8; 32], data: &[u8]) -> anyhow::Result<Vec<u8>> {
    if data.len() < 12 + 16 {
        return Err(anyhow::anyhow!("Data too short for decryption"));
    }

    let nonce = Nonce::from_slice(&data[..12]);
    let ciphertext = &data[12..];

    let cipher_key = Key::<Aes256Gcm>::from_slice(key);
    let cipher = Aes256Gcm::new(cipher_key);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("Decryption failed: {}", e))?;

    Ok(plaintext)
}
