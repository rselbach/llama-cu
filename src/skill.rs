//! The agent skill that teaches agents to use llama-cu. It is built into the
//! binary, so every install can set it up and it matches the binary.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, ErrorCode, Result};

const SKILL: &str = include_str!("../skills/llama-cu/SKILL.md");

/// Result of `install-skill`.
#[derive(Serialize)]
pub struct InstalledSkill {
    pub path: PathBuf,
}

/// Writes the skill to `<dir>/llama-cu/SKILL.md`, replacing an older copy.
/// `dir` defaults to `~/.agents/skills`, where Pi and Codex look.
pub fn install(dir: Option<&Path>) -> Result<InstalledSkill> {
    let dir = match dir {
        Some(dir) => std::path::absolute(dir)?,
        None => default_dir()?,
    };
    let skill_dir = dir.join("llama-cu");
    // `just install-skill` links a checkout here; writing through the link
    // would overwrite the checkout's copy.
    if fs::symlink_metadata(&skill_dir).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            format!(
                "{} is a symbolic link; remove it to install a copy",
                skill_dir.display()
            ),
        ));
    }
    fs::create_dir_all(&skill_dir)?;
    let path = skill_dir.join("SKILL.md");
    fs::write(&path, SKILL)?;
    Ok(InstalledSkill { path })
}

fn default_dir() -> Result<PathBuf> {
    let home = std::env::home_dir().ok_or_else(|| {
        Error::new(
            ErrorCode::InvalidArgument,
            "cannot find the home directory; pass --dir",
        )
    })?;
    Ok(home.join(".agents/skills"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns an empty directory for one test.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("llama-cu-skill-{name}-{}", std::process::id()));
        match fs::remove_dir_all(&dir) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                panic!("clear scratch directory: {err}")
            }
            _ => {}
        }
        fs::create_dir_all(&dir).expect("create scratch directory");
        dir
    }

    #[test]
    fn installs_and_replaces_the_skill() {
        let dir = scratch("install");
        let want = dir.join("llama-cu/SKILL.md");
        let installed = install(Some(&dir)).expect("install");
        assert_eq!(installed.path, want);
        assert_eq!(fs::read_to_string(&want).expect("read"), SKILL);

        fs::write(&want, "Greendale Community College").expect("write old copy");
        install(Some(&dir)).expect("reinstall");
        assert_eq!(fs::read_to_string(&want).expect("read"), SKILL);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_linked_skill_directory() {
        let dir = scratch("linked");
        let checkout = dir.join("checkout");
        fs::create_dir(&checkout).expect("create checkout");
        fs::write(checkout.join("SKILL.md"), "Troy Barnes").expect("write checkout copy");
        std::os::unix::fs::symlink(&checkout, dir.join("llama-cu")).expect("link");

        let err = install(Some(&dir))
            .err()
            .expect("linked directory must fail");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        let kept = fs::read_to_string(checkout.join("SKILL.md")).expect("read");
        assert_eq!(kept, "Troy Barnes");
        fs::remove_dir_all(&dir).expect("clean up");
    }
}
