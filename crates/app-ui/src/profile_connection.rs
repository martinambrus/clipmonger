//! Bounded profile credentials and rendezvous resolution.
use std::io::Read;
use std::path::{Path, PathBuf};

/// Fresh connection information for one profile connection.
pub struct ProfileConnection {
    pub endpoint: PathBuf,
    pub token: String,
}

/// Profile data is unavailable or invalid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileError;

fn read_bounded(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, ProfileError> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ProfileError),
    };
    if !meta.is_file() || meta.len() > limit {
        return Err(ProfileError);
    }
    let file = std::fs::File::open(path).map_err(|_| ProfileError)?;
    if !file.metadata().map_err(|_| ProfileError)?.is_file() {
        return Err(ProfileError);
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ProfileError)?;
    if bytes.len() as u64 > limit {
        return Err(ProfileError);
    }
    Ok(Some(bytes))
}

/// Resolve credentials anew; filesystem calls themselves are synchronous.
///
/// # Errors
/// Returns `ProfileError` for missing, invalid, or unreadable credentials.
pub fn resolve(profile: &Path) -> Result<ProfileConnection, ProfileError> {
    let token = read_bounded(&profile.join("token"), 4096)?.ok_or(ProfileError)?;
    let token = std::str::from_utf8(&token)
        .map_err(|_| ProfileError)?
        .trim();
    if token.is_empty() {
        return Err(ProfileError);
    }
    let rendezvous = read_bounded(&profile.join("rendezvous.json"), 16384)?;
    let endpoint = rendezvous
        .as_deref()
        .and_then(|text| serde_json::from_slice::<serde_json::Value>(text).ok())
        .and_then(|body| {
            body.get("socket")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty() && !s.contains('\0'))
                .map(PathBuf::from)
        });
    #[cfg(windows)]
    let endpoint = {
        let path = endpoint.ok_or(ProfileError)?;
        let text = path.to_str().ok_or(ProfileError)?;
        let name = text.strip_prefix(r"\\.\pipe\").ok_or(ProfileError)?;
        if name.is_empty() || name.contains(['\\', '/']) {
            return Err(ProfileError);
        }
        path
    };
    #[cfg(not(windows))]
    let endpoint = endpoint.unwrap_or_else(|| {
        let runtime =
            std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
        clipmonger_ipc::framing::socket_path_for(profile, &runtime)
    });
    Ok(ProfileConnection {
        endpoint,
        token: token.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Profile(PathBuf);
    impl Profile {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "cm-profile-{}-{}",
                std::process::id(),
                uuid::Uuid::now_v7()
            ));
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("token"), " token \n").unwrap();
            Self(path)
        }
    }
    impl Drop for Profile {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    #[test]
    fn credentials_are_bounded_and_refreshed() {
        let p = Profile::new();
        #[cfg(unix)]
        {
            assert_eq!(resolve(&p.0).unwrap().token, "token");
            std::fs::write(p.0.join("token"), "next").unwrap();
            assert_eq!(resolve(&p.0).unwrap().token, "next");
            let runtime =
                std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let fallback = clipmonger_ipc::framing::socket_path_for(&p.0, &runtime);
            for invalid in [&[0xff][..], &b"{invalid"[..]] {
                std::fs::write(p.0.join("rendezvous.json"), invalid).unwrap();
                let connection = resolve(&p.0).unwrap();
                assert_eq!(connection.token, "next");
                assert_eq!(connection.endpoint, fallback);
            }
        }
        #[cfg(windows)]
        {
            std::fs::write(
                p.0.join("rendezvous.json"),
                serde_json::json!({"socket":r"\\.\pipe\clipmonger-test"}).to_string(),
            )
            .unwrap();
            assert!(resolve(&p.0).is_ok());
        }
        std::fs::write(p.0.join("token"), [0xff]).unwrap();
        assert!(resolve(&p.0).is_err());
        std::fs::write(p.0.join("token"), vec![b'x'; 4097]).unwrap();
        assert!(resolve(&p.0).is_err());
        std::fs::write(p.0.join("token"), "valid").unwrap();
        std::fs::write(p.0.join("rendezvous.json"), vec![b'x'; 16385]).unwrap();
        assert!(resolve(&p.0).is_err());
        std::fs::remove_file(p.0.join("rendezvous.json")).unwrap();
        std::fs::create_dir(p.0.join("rendezvous.json")).unwrap();
        assert!(resolve(&p.0).is_err());
    }
    #[cfg(windows)]
    #[test]
    fn only_local_pipe_rendezvous_is_accepted() {
        let p = Profile::new();
        for endpoint in [r"\\server\pipe\name", r"\\.\pipe\", r"\\.\pipe\nested\name"] {
            std::fs::write(
                p.0.join("rendezvous.json"),
                serde_json::json!({"socket":endpoint}).to_string(),
            )
            .unwrap();
            assert!(resolve(&p.0).is_err());
        }
        std::fs::write(
            p.0.join("rendezvous.json"),
            serde_json::json!({"socket":r"\\.\pipe\clipmonger-test"}).to_string(),
        )
        .unwrap();
        assert!(resolve(&p.0).is_ok());
    }
}
