//! Check the CLI input policy without opening apps or posting events.

use std::process::Command;

#[test]
fn foreground_input_requires_an_explicit_opt_in() {
    let cases: &[(&str, &[&str], Option<&str>, bool)] = &[
        ("default", &[], None, true),
        ("background flag", &["--background"], None, true),
        ("foreground flag", &["--foreground"], None, false),
        ("background environment", &[], Some("true"), true),
        ("legacy foreground environment", &[], Some("false"), false),
        ("empty foreground environment", &[], Some(""), false),
        ("short foreground environment", &[], Some("n"), false),
        ("case insensitive environment", &[], Some("OFF"), false),
        (
            "flag overrides environment",
            &["--background"],
            Some("false"),
            true,
        ),
        (
            "foreground overrides environment",
            &["--foreground"],
            Some("true"),
            false,
        ),
    ];
    let dir = std::env::temp_dir().join(format!("llama-cu-input-mode-{}", std::process::id()));
    for &(name, flags, background_env, background) in cases {
        // Paste is rejected before looking for an app in background mode.
        // Foreground mode reaches app selection, then fails without any input.
        for split in 0..=flags.len() {
            let mut command = Command::new(env!("CARGO_BIN_EXE_llama-cu"));
            command
                .env_remove("LLAMA_CU_BACKGROUND")
                .env("LLAMA_CU_STATE_DIR", &dir)
                .env("LLAMA_CU_SESSION", "probe")
                .arg("--json")
                .args(&flags[..split])
                .args(["paste", "Troy Barnes"])
                .args(&flags[split..]);
            if let Some(value) = background_env {
                command.env("LLAMA_CU_BACKGROUND", value);
            }
            let output = command.output().expect("run CLI");
            assert_eq!(output.status.code(), Some(1), "{name}");
            let error: serde_json::Value =
                serde_json::from_slice(&output.stderr).unwrap_or_else(|err| {
                    panic!("{name}: {err}: {}", String::from_utf8_lossy(&output.stderr))
                });
            let want = if background {
                "background_unavailable"
            } else {
                "no_app_selected"
            };
            assert_eq!(error["error"]["code"], want, "{name}");
        }
    }
    std::fs::remove_dir_all(dir).expect("remove test session");
}
