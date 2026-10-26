//! JSONL framing: line codec, base64 chunking, hashing, socket paths.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::proto::{Envelope, IpcError, CHUNK_RAW_BYTES, IPC_VERSION, MAX_LINE_BYTES};

/// One base64 chunk of a blob upload stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    /// Chunk index, starting at 0 and contiguous.
    pub seq: u64,
    /// Standard padded base64 of this chunk's raw bytes.
    pub data_b64: String,
    /// True on the terminal chunk of the stream.
    pub eof: bool,
    /// Total-stream SHA-256 hex, present on the terminal chunk only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// Serialize an envelope to one JSONL line (trailing newline included).
///
/// # Errors
///
/// Returns an error when the line exceeds `MAX_LINE_BYTES` or serialization fails.
pub fn encode_line(env: &Envelope) -> Result<Vec<u8>, IpcError> {
    let mut bytes =
        serde_json::to_vec(env).map_err(|e| IpcError::new("encode_failed", e.to_string()))?;
    if bytes.len() > MAX_LINE_BYTES {
        let len = bytes.len();
        return Err(IpcError::new(
            "oversize",
            format!("line {len} bytes exceeds cap {MAX_LINE_BYTES}"),
        ));
    }
    bytes.push(b'\n');
    Ok(bytes)
}

/// Parse one JSONL line into an envelope.
///
/// # Errors
///
/// Returns an error for oversize lines, malformed JSON, or version mismatch.
pub fn decode_line(line: &[u8]) -> Result<Envelope, IpcError> {
    let mut end = line.len();
    while end > 0 && (line[end - 1] == b'\n' || line[end - 1] == b'\r') {
        end -= 1;
    }
    let body = &line[..end];
    if body.len() > MAX_LINE_BYTES {
        let len = body.len();
        return Err(IpcError::new(
            "oversize",
            format!("line {len} bytes exceeds cap {MAX_LINE_BYTES}"),
        ));
    }
    let env: Envelope =
        serde_json::from_slice(body).map_err(|e| IpcError::new("decode_failed", e.to_string()))?;
    if env.v != IPC_VERSION {
        return Err(IpcError::new(
            "version_mismatch",
            format!("expected {IPC_VERSION}, got {}", env.v),
        ));
    }
    Ok(env)
}

/// Split raw bytes into base64 chunks; the terminal chunk carries the total hash.
#[must_use]
pub fn chunk_blob(bytes: &[u8]) -> Vec<Chunk> {
    let total = hex_digest(&sha256(bytes));
    if bytes.is_empty() {
        return vec![Chunk {
            seq: 0,
            data_b64: String::new(),
            eof: true,
            sha256: Some(total),
        }];
    }
    let count = bytes.len().div_ceil(CHUNK_RAW_BYTES);
    let mut chunks = Vec::with_capacity(count);
    let mut seq: u64 = 0;
    for piece in bytes.chunks(CHUNK_RAW_BYTES) {
        let last = chunks.len() + 1 == count;
        chunks.push(Chunk {
            seq,
            data_b64: b64_encode(piece),
            eof: last,
            sha256: if last { Some(total.clone()) } else { None },
        });
        seq = seq.wrapping_add(1);
    }
    chunks
}

/// Stable 16-char lowercase hex id for a profile dir (SHA-256 prefix).
#[must_use]
pub fn profile_hash(profile: &Path) -> String {
    hex_digest(&sha256(profile.as_os_str().as_encoded_bytes()))[..16].to_string()
}

/// Named-pipe name for a profile on Windows.
#[must_use]
pub fn pipe_name_for(profile_dir: &Path) -> String {
    format!(r"\\.\pipe\clipmonger-{}", profile_hash(profile_dir))
}

/// Socket path for a profile; long profile paths fall back under `runtime_dir`.
#[must_use]
pub fn socket_path_for(profile_dir: &Path, runtime_dir: &Path) -> PathBuf {
    if profile_dir.as_os_str().as_encoded_bytes().len() > 100 {
        let hash = profile_hash(profile_dir);
        runtime_dir.join(format!("clipmonger-{hash}.sock"))
    } else {
        profile_dir.join("clipmonger.sock")
    }
}

/// RFC 4648 standard padded base64 encoding.
fn b64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3).saturating_mul(4));
    let (triples, remainder) = data.as_chunks::<3>();
    for triple in triples {
        let b0 = triple[0];
        let b1 = triple[1];
        let b2 = triple[2];
        out.push(char::from(ALPHABET[(b0 >> 2) as usize]));
        out.push(char::from(
            ALPHABET[((((b0 & 0x03) << 4) | (b1 >> 4)) & 0x3F) as usize],
        ));
        out.push(char::from(
            ALPHABET[((((b1 & 0x0F) << 2) | (b2 >> 6)) & 0x3F) as usize],
        ));
        out.push(char::from(ALPHABET[(b2 & 0x3F) as usize]));
    }
    if remainder.len() == 1 {
        let b0 = remainder[0];
        out.push(char::from(ALPHABET[(b0 >> 2) as usize]));
        out.push(char::from(ALPHABET[(((b0 & 0x03) << 4) & 0x3F) as usize]));
        out.push_str("==");
    } else if remainder.len() == 2 {
        let b0 = remainder[0];
        let b1 = remainder[1];
        out.push(char::from(ALPHABET[(b0 >> 2) as usize]));
        out.push(char::from(
            ALPHABET[((((b0 & 0x03) << 4) | (b1 >> 4)) & 0x3F) as usize],
        ));
        out.push(char::from(ALPHABET[(((b1 & 0x0F) << 2) & 0x3F) as usize]));
        out.push('=');
    }
    out
}

/// Lowercase hex of a 32-byte digest.
fn hex_digest(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(char::from(HEX[(byte >> 4) as usize]));
        out.push(char::from(HEX[(byte & 0x0F) as usize]));
    }
    out
}

/// Self-contained FIPS 180-4 SHA-256 over bytes.
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    let mut padded = data.to_vec();
    let bit_len = u64::try_from(data.len())
        .unwrap_or(u64::MAX)
        .wrapping_mul(8);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    let (blocks, _) = padded.as_chunks::<64>();
    for block in blocks {
        sha256_compress(&mut state, block, &K);
    }
    let mut out = [0_u8; 32];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// FIPS 180-4 compression of one 64-byte block into the running state.
fn sha256_compress(state: &mut [u32; 8], block: &[u8; 64], k: &[u32; 64]) {
    let mut schedule = [0_u32; 64];
    let (words, _) = block.as_chunks::<4>();
    for (i, word) in words.iter().enumerate().take(16) {
        schedule[i] = u32::from_be_bytes(*word);
    }
    for i in 16..64 {
        let s0 = schedule[i - 15].rotate_right(7)
            ^ schedule[i - 15].rotate_right(18)
            ^ (schedule[i - 15] >> 3);
        let s1 = schedule[i - 2].rotate_right(17)
            ^ schedule[i - 2].rotate_right(19)
            ^ (schedule[i - 2] >> 10);
        schedule[i] = schedule[i - 16]
            .wrapping_add(s0)
            .wrapping_add(schedule[i - 7])
            .wrapping_add(s1);
    }
    let (mut wa, mut wb, mut wc, mut wd, mut we, mut wf, mut wg, mut wh) = (
        state[0], state[1], state[2], state[3], state[4], state[5], state[6], state[7],
    );
    for i in 0..64 {
        let s1 = we.rotate_right(6) ^ we.rotate_right(11) ^ we.rotate_right(25);
        let ch = (we & wf) ^ ((!we) & wg);
        let t1 = wh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(k[i])
            .wrapping_add(schedule[i]);
        let s0 = wa.rotate_right(2) ^ wa.rotate_right(13) ^ wa.rotate_right(22);
        let maj = (wa & wb) ^ (wa & wc) ^ (wb & wc);
        let t2 = s0.wrapping_add(maj);
        wh = wg;
        wg = wf;
        wf = we;
        we = wd.wrapping_add(t1);
        wd = wc;
        wc = wb;
        wb = wa;
        wa = t1.wrapping_add(t2);
    }
    state[0] = state[0].wrapping_add(wa);
    state[1] = state[1].wrapping_add(wb);
    state[2] = state[2].wrapping_add(wc);
    state[3] = state[3].wrapping_add(wd);
    state[4] = state[4].wrapping_add(we);
    state[5] = state[5].wrapping_add(wf);
    state[6] = state[6].wrapping_add(wg);
    state[7] = state[7].wrapping_add(wh);
}
