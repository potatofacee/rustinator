use std::io::Write;
use std::path::PathBuf;

const ZSH_ZSHENV: &str = r#"
if [ -n "$__RUSTINATOR_ORIG_ZDOTDIR" ]; then
    [ -f "$__RUSTINATOR_ORIG_ZDOTDIR/.zshenv" ] && . "$__RUSTINATOR_ORIG_ZDOTDIR/.zshenv"
elif [ -f "$HOME/.zshenv" ]; then
    . "$HOME/.zshenv"
fi
"#;

const ZSH_ZPROFILE: &str = r#"
if [ -n "$__RUSTINATOR_ORIG_ZDOTDIR" ]; then
    [ -f "$__RUSTINATOR_ORIG_ZDOTDIR/.zprofile" ] && . "$__RUSTINATOR_ORIG_ZDOTDIR/.zprofile"
elif [ -f "$HOME/.zprofile" ]; then
    . "$HOME/.zprofile"
fi
"#;

const ZSH_ZSHRC: &str = r#"
# rustinator shell integration — sets window title via OSC 0
__rustinator_precmd() {
    printf '\e]0;%s@%s: %s\a' "$USER" "${HOST%%.*}" "${PWD/#$HOME/~}"
}
autoload -Uz add-zsh-hook
add-zsh-hook precmd __rustinator_precmd
# Source the user's real zshrc.
if [ -n "$__RUSTINATOR_ORIG_ZDOTDIR" ]; then
    ZDOTDIR="$__RUSTINATOR_ORIG_ZDOTDIR"
    unset __RUSTINATOR_ORIG_ZDOTDIR
    [ -f "$ZDOTDIR/.zshrc" ] && . "$ZDOTDIR/.zshrc"
elif [ -f "$HOME/.zshrc" ]; then
    . "$HOME/.zshrc"
fi
"#;

const ZSH_ZLOGIN: &str = r#"
if [ -n "$__RUSTINATOR_ORIG_ZDOTDIR" ]; then
    [ -f "$__RUSTINATOR_ORIG_ZDOTDIR/.zlogin" ] && . "$__RUSTINATOR_ORIG_ZDOTDIR/.zlogin"
elif [ -f "$HOME/.zlogin" ]; then
    . "$HOME/.zlogin"
fi
"#;

const BASH_INTEGRATION: &str = r#"
# rustinator shell integration — sets window title via OSC 0
__rustinator_prompt_command() {
    printf '\e]0;%s@%s: %s\a' "$USER" "${HOSTNAME%%.*}" "${PWD/#$HOME/\~}"
}
PROMPT_COMMAND="__rustinator_prompt_command${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
# Source the user's profile and rc files.
if shopt -q login_shell 2>/dev/null; then
    [ -f /etc/profile ] && . /etc/profile
    if [ -f "$HOME/.bash_profile" ]; then
        . "$HOME/.bash_profile"
    elif [ -f "$HOME/.bash_login" ]; then
        . "$HOME/.bash_login"
    elif [ -f "$HOME/.profile" ]; then
        . "$HOME/.profile"
    fi
else
    [ -f "$HOME/.bashrc" ] && . "$HOME/.bashrc"
fi
"#;

fn integration_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("rustinator-shell-integration");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn write_file(dir: &std::path::Path, name: &str, content: &str) {
    if let Ok(mut f) = std::fs::File::create(dir.join(name)) {
        let _ = f.write_all(content.as_bytes());
    }
}

pub fn write_scripts() -> PathBuf {
    let dir = integration_dir();
    write_file(&dir, ".zshenv", ZSH_ZSHENV);
    write_file(&dir, ".zprofile", ZSH_ZPROFILE);
    write_file(&dir, ".zshrc", ZSH_ZSHRC);
    write_file(&dir, ".zlogin", ZSH_ZLOGIN);
    write_file(&dir, "bashrc", BASH_INTEGRATION);
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
    fn write_scripts_creates_all_zsh_files() {
        let dir = write_scripts();
        assert!(dir.join(".zshenv").exists());
        assert!(dir.join(".zprofile").exists());
        assert!(dir.join(".zshrc").exists());
        assert!(dir.join(".zlogin").exists());
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

    #[test]
    fn zprofile_sources_user_zprofile() {
        assert!(ZSH_ZPROFILE.contains(".zprofile"));
    }

    #[test]
    fn bash_integration_handles_login_shell() {
        assert!(BASH_INTEGRATION.contains("login_shell"));
        assert!(BASH_INTEGRATION.contains(".bash_profile"));
        assert!(BASH_INTEGRATION.contains(".bashrc"));
    }

    // Gap #22: custom command support
    #[test]
    #[ignore = "gap #22: custom command not yet supported in shell spawning"]
    fn custom_command_bypasses_default_shell() {
        panic!("custom command path not yet implemented in Pane::spawn");
    }
}
