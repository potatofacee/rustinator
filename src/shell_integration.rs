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
