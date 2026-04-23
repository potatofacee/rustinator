use std::io::Write;
use std::path::PathBuf;

const ZSH_INTEGRATION: &str = r#"
# rustinator shell integration — sets window title via OSC 0
__rustinator_precmd() {
    printf '\e]0;%s@%s: %s\a' "$USER" "${HOST%%.*}" "${PWD/#$HOME/~}"
}
autoload -Uz add-zsh-hook
add-zsh-hook precmd __rustinator_precmd
# Source the user's real zshrc if ZDOTDIR was overridden.
if [ -n "$__RUSTINATOR_ORIG_ZDOTDIR" ]; then
    ZDOTDIR="$__RUSTINATOR_ORIG_ZDOTDIR"
    unset __RUSTINATOR_ORIG_ZDOTDIR
    [ -f "$ZDOTDIR/.zshrc" ] && . "$ZDOTDIR/.zshrc"
elif [ -f "$HOME/.zshrc" ]; then
    . "$HOME/.zshrc"
fi
"#;

const BASH_INTEGRATION: &str = r#"
# rustinator shell integration — sets window title via OSC 0
__rustinator_prompt_command() {
    printf '\e]0;%s@%s: %s\a' "$USER" "${HOSTNAME%%.*}" "${PWD/#$HOME/\~}"
}
PROMPT_COMMAND="__rustinator_prompt_command${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
# Source the user's real bashrc.
[ -f "$HOME/.bashrc" ] && . "$HOME/.bashrc"
"#;

fn integration_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("rustinator-shell-integration");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn write_scripts() -> PathBuf {
    let dir = integration_dir();

    let zshrc = dir.join(".zshrc");
    if let Ok(mut f) = std::fs::File::create(&zshrc) {
        let _ = f.write_all(ZSH_INTEGRATION.as_bytes());
    }

    let bashrc = dir.join("bashrc");
    if let Ok(mut f) = std::fs::File::create(&bashrc) {
        let _ = f.write_all(BASH_INTEGRATION.as_bytes());
    }

    dir
}

pub fn inject_env(env: &mut std::collections::HashMap<String, String>) {
    let dir = write_scripts();

    if let Ok(existing) = std::env::var("ZDOTDIR") {
        env.insert("__RUSTINATOR_ORIG_ZDOTDIR".into(), existing);
    }
    env.insert("ZDOTDIR".into(), dir.to_string_lossy().into_owned());
    env.insert(
        "BASH_ENV".into(),
        dir.join("bashrc").to_string_lossy().into_owned(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_scripts_creates_zshrc() {
        let dir = write_scripts();
        assert!(dir.join(".zshrc").exists());
    }

    #[test]
    fn write_scripts_creates_bashrc() {
        let dir = write_scripts();
        assert!(dir.join("bashrc").exists());
    }

    #[test]
    fn inject_env_sets_zdotdir() {
        let mut env = std::collections::HashMap::new();
        inject_env(&mut env);
        assert!(env.contains_key("ZDOTDIR"));
        assert!(env.contains_key("BASH_ENV"));
    }

    // ── Gap inventory guardrails ──────────────────────────────────────

    // Gap #24: login shell support
    #[test]
    #[ignore = "gap #24: login shell flag not yet supported in shell spawning"]
    fn inject_env_login_shell_flag() {
        let mut env = std::collections::HashMap::new();
        inject_env(&mut env);
        // When login_shell is true, the shell should be invoked with -l
        // or the argv[0] should be prefixed with "-".
        // This tests that the mechanism exists — actual behavior tested at spawn level.
        assert!(env.contains_key("RUSTINATOR_LOGIN_SHELL")
            || true, "login shell mechanism should be testable");
        panic!("login shell support not yet wired into shell spawning");
    }

    // Gap #22: custom command support
    #[test]
    #[ignore = "gap #22: custom command not yet supported in shell spawning"]
    fn custom_command_bypasses_default_shell() {
        // When custom_command is set, Pane::spawn should use that instead
        // of the user's login shell. This is a placeholder that proves
        // the path exists.
        panic!("custom command path not yet implemented in Pane::spawn");
    }
}
