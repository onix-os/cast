use sha2::{Digest, Sha256};

const HASH_CHECKPOINT_BYTES: usize = 64 * 1024;

pub(crate) fn sha256_checked<E>(
    bytes: &[u8],
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<String, E> {
    checkpoint()?;
    let mut digest = Sha256::new();
    for chunk in bytes.chunks(HASH_CHECKPOINT_BYTES) {
        digest.update(chunk);
        checkpoint()?;
    }
    let sha256 = format!("{:x}", digest.finalize());
    checkpoint()?;
    Ok(sha256)
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use super::*;

    #[test]
    fn hashing_checks_before_each_64_kib_chunk_and_after_finalization() {
        for (bytes, expected_checkpoints) in [
            (0, 2),
            (1, 3),
            (HASH_CHECKPOINT_BYTES, 3),
            (HASH_CHECKPOINT_BYTES + 1, 4),
        ] {
            let mut checkpoints = 0;
            let mut checkpoint = || {
                checkpoints += 1;
                Ok::<(), Infallible>(())
            };
            let sha256 = sha256_checked(&vec![b'x'; bytes], &mut checkpoint).unwrap();

            assert_eq!(sha256.len(), 64);
            assert_eq!(checkpoints, expected_checkpoints);
        }
    }
}
