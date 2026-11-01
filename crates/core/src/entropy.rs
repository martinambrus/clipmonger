//! Fallible entropy drawn directly from the operating system.

pub use getrandom::Error;

/// Draw a fresh 64-bit value from the operating system.
///
/// # Errors
/// Returns the operating system entropy error without a fallback.
pub fn random_u64() -> Result<u64, Error> {
    random_u64_with(getrandom::fill)
}

fn random_u64_with(fill: impl FnOnce(&mut [u8]) -> Result<(), Error>) -> Result<u64, Error> {
    let mut bytes = [0; 8];
    fill(&mut bytes)?;
    Ok(u64::from_ne_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returned_bytes_are_used() {
        let value = 0x0123_4567_89ab_cdef_u64;
        assert_eq!(
            random_u64_with(|bytes| {
                bytes.copy_from_slice(&value.to_ne_bytes());
                Ok(())
            })
            .unwrap(),
            value
        );
    }

    #[test]
    fn entropy_error_propagates() {
        let result = random_u64_with(|_| Err(Error::UNSUPPORTED));
        assert_eq!(result.unwrap_err(), Error::UNSUPPORTED);
    }
}
