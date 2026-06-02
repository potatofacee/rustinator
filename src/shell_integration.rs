use std::io::Write;
use std::path::PathBuf;

// We hijack ZDOTDIR to inject a title hook without touching the user's
// dotfiles. The critical rule: point ZDOTDIR back at the user's real config
// dir *while sourcing each of their files*, so any nested zsh started during
// startup (version managers, compinit, instant-prompt subshells, etc.) sees
// the real ZDOTDIR and does NOT re-enter this integration — re-entry caused
// infinite recursion ("recursion limit exceeded / job table full"). After
// sourcing, restore ZDOTDIR to the integration dir so zsh keeps reading our
// remaining startup files. __RUSTINATOR_ORIG_ZDOTDIR holds the user's real
// dir (set by inject_env); it falls back to $HOME when unset.

// $ZDOTDIR on entry to each file is this integration dir (that's how zsh found
// the file). __rustinator_user resolves the user's real config dir; if a stale
// or inherited value points it back at the integration dir, fall back to $HOME
// so we can never source ourselves (which caused infinite recursion).

const ZSH_ZSHENV: &str = r#"
__rustinator_int="$ZDOTDIR"
__rustinator_user="${__RUSTINATOR_ORIG_ZDOTDIR:-$HOME}"
[ "$__rustinator_user" = "$__rustinator_int" ] && __rustinator_user="$HOME"
ZDOTDIR="$__rustinator_user"
[ -f "$ZDOTDIR/.zshenv" ] && . "$ZDOTDIR/.zshenv"
__RUSTINATOR_ORIG_ZDOTDIR="$ZDOTDIR"
ZDOTDIR="$__rustinator_int"
unset __rustinator_int __rustinator_user
"#;

const ZSH_ZPROFILE: &str = r#"
__rustinator_int="$ZDOTDIR"
__rustinator_user="${__RUSTINATOR_ORIG_ZDOTDIR:-$HOME}"
[ "$__rustinator_user" = "$__rustinator_int" ] && __rustinator_user="$HOME"
ZDOTDIR="$__rustinator_user"
[ -f "$ZDOTDIR/.zprofile" ] && . "$ZDOTDIR/.zprofile"
__RUSTINATOR_ORIG_ZDOTDIR="$ZDOTDIR"
ZDOTDIR="$__rustinator_int"
unset __rustinator_int __rustinator_user
"#;

const ZSH_ZSHRC: &str = r#"
# rustinator shell integration — sets window title via OSC 0
__rustinator_precmd() {
    printf '\e]0;%s@%s: %s\a' "$USER" "${HOST%%.*}" "${PWD/#$HOME/~}"
}
autoload -Uz add-zsh-hook
add-zsh-hook precmd __rustinator_precmd
# Source the user's real zshrc with their ZDOTDIR, then hand the session back
# to their config dir (so $ZDOTDIR is clean and any later .zlogin is read from
# the user's dir directly).
__rustinator_int="$ZDOTDIR"
__rustinator_user="${__RUSTINATOR_ORIG_ZDOTDIR:-$HOME}"
[ "$__rustinator_user" = "$__rustinator_int" ] && __rustinator_user="$HOME"
ZDOTDIR="$__rustinator_user"
[ -f "$ZDOTDIR/.zshrc" ] && . "$ZDOTDIR/.zshrc"
unset __rustinator_int __rustinator_user __RUSTINATOR_ORIG_ZDOTDIR
"#;

const ZSH_ZLOGIN: &str = r#"
__rustinator_int="$ZDOTDIR"
__rustinator_user="${__RUSTINATOR_ORIG_ZDOTDIR:-$HOME}"
[ "$__rustinator_user" = "$__rustinator_int" ] && __rustinator_user="$HOME"
ZDOTDIR="$__rustinator_user"
[ -f "$ZDOTDIR/.zlogin" ] && . "$ZDOTDIR/.zlogin"
unset __rustinator_int __rustinator_user __RUSTINATOR_ORIG_ZDOTDIR
"#;

const BASH_INTEGRATION: &str = r#"
# rustinator shell integration — sets window title via OSC 0.
# BASH_ENV is inherited by nested non-interactive bash, which re-sources this
# file; guard against re-entry so sourcing the user's rc (and anything it
# spawns) can't recurse.
if [ -n "$__RUSTINATOR_BASH_ACTIVE" ]; then
    return 2>/dev/null
fi
export __RUSTINATOR_BASH_ACTIVE=1
__rustinator_prompt_command() {
    printf '\e]0;%s@%s: %s\a' "$USER" "${HOSTNAME%%.*}" "${PWD/#$HOME/~}"
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
unset __RUSTINATOR_BASH_ACTIVE
"#;

fn integration_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("rustinator-shell-integration");
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
    let dir_str = dir.to_string_lossy().into_owned();

    // Capture the user's real ZDOTDIR so the scripts can source their dotfiles.
    // But if we inherited our own integration dir (e.g. launched from a shell
    // whose session still has ZDOTDIR pointed here), do NOT record it as the
    // original — that would make the scripts source themselves and recurse.
    if let Ok(existing) = std::env::var("ZDOTDIR") {
        if existing != dir_str {
            env.insert("__RUSTINATOR_ORIG_ZDOTDIR".into(), existing);
        }
    }
    env.insert("ZDOTDIR".into(), dir_str);
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
