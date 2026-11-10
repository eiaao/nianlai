use crate::content::Fingerprint;

// 固定 SHA-256；工具和非 Windows 测试不依赖系统加密库。
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const INIT: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

fn compress(chunk: &[u8; 64], state: &mut [u32; 8]) {
    let mut w = [0u32; 64];
    for (word, part) in w.iter_mut().zip(chunk.as_chunks::<4>().0) {
        *word = u32::from_be_bytes(*part);
    }
    for i in 16..64 {
        let a = w[i - 15];
        let b = w[i - 2];
        w[i] = w[i - 16]
            .wrapping_add(a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3))
            .wrapping_add(w[i - 7])
            .wrapping_add(b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10));
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for i in 0..64 {
        let t1 = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (s, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *s = s.wrapping_add(value);
    }
}

/// 流式 SHA-256：备份导出和导入按块读文件，不必把几个 GB 的库整个读进内存。
pub struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    filled: usize,
    total: u64,
}
impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}
impl Sha256 {
    pub fn new() -> Self {
        Self {
            state: INIT,
            buffer: [0; 64],
            filled: 0,
            total: 0,
        }
    }
    pub fn update(&mut self, mut bytes: &[u8]) {
        self.total += bytes.len() as u64;
        if self.filled > 0 {
            let take = (64 - self.filled).min(bytes.len());
            self.buffer[self.filled..self.filled + take].copy_from_slice(&bytes[..take]);
            self.filled += take;
            bytes = &bytes[take..];
            if self.filled < 64 {
                return;
            }
            let block = self.buffer;
            compress(&block, &mut self.state);
            self.filled = 0;
        }
        let (blocks, rest) = bytes.as_chunks::<64>();
        for block in blocks {
            compress(block, &mut self.state);
        }
        self.buffer[..rest.len()].copy_from_slice(rest);
        self.filled = rest.len();
    }
    pub fn finish(mut self) -> Fingerprint {
        let mut tail = [0u8; 128];
        tail[..self.filled].copy_from_slice(&self.buffer[..self.filled]);
        tail[self.filled] = 0x80;
        let size = if self.filled < 56 { 64 } else { 128 };
        tail[size - 8..size].copy_from_slice(&(self.total * 8).to_be_bytes());
        for block in tail[..size].as_chunks::<64>().0 {
            compress(block, &mut self.state);
        }
        let mut digest = [0; 32];
        for (part, value) in digest.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            *part = value.to_be_bytes();
        }
        digest
    }
}

pub fn sha256(bytes: &[u8]) -> Result<Fingerprint, String> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(hasher.finish())
}

pub fn hex(fingerprint: &[u8]) -> String {
    fingerprint.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_matches_one_shot_and_known_vectors() {
        assert_eq!(
            hex(&sha256(b"abc").unwrap()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7) as u8).collect();
        let whole = sha256(&data).unwrap();
        for step in [1, 13, 55, 56, 63, 64, 65, 200] {
            let mut hasher = Sha256::new();
            for chunk in data.chunks(step) {
                hasher.update(chunk);
            }
            assert_eq!(hasher.finish(), whole, "step {step}");
        }
    }
}
