# clackatoo's shell integration for zsh, part two, loaded by its .zshenv
# in an interactive shell, before the user's .zshrc. Its hooks go in after
# .zshrc, before the first prompt.
#
# It marks where each prompt starts (OSC 133 A) and ends (B), where a
# command's output starts (C, with the command) and how the command exited
# (D), and says where the shell is (OSC 7), so clackatoo can tell commands
# apart, and knows when typing is read by the shell. See
# `clackatoo_term::blocks`.

[[ -z ${__clackatoo_integrated-} ]] || return 0
typeset -g __clackatoo_integrated=1

# Marks are written to the terminal itself, not to stdout: powerlevel10k's
# instant prompt points stdout at a file while .zshrc runs, and would
# report anything written there as output it didn't expect. Opened now,
# before .zshrc, and closed in the programs the shell runs, unless zsh
# was built without zsh/system, when they get it too.
typeset -gi __clackatoo_fd=1
() {
    emulate -L zsh
    [[ -w $TTY ]] || return
    local -i fd
    if zmodload zsh/system 2>/dev/null; then
        sysopen -o cloexec -wu fd -- $TTY 2>/dev/null && __clackatoo_fd=fd
    else
        { exec {fd}>>$TTY } 2>/dev/null && __clackatoo_fd=fd
    fi
}

# First of the prompt hooks: how the command exited, and where the shell
# is now. A directory's `%` is written `%25`, as in a URL.
__clackatoo_precmd() {
    local -i ret=$?
    emulate -L zsh
    print -rnu $__clackatoo_fd -- \
        $'\e]133;D;'$ret$'\a\e]7;file://'$HOST${PWD//\%/%25}$'\a'
    return ret
}

# Last of the prompt hooks, after anything that sets PS1 afresh for each
# prompt, as many themes do. Hooks added since the last prompt, as
# .zshrc adds them before the first, are moved ahead of it for the next,
# and for this one the mark waits until they've run, as powerlevel10k's
# instant prompt waits: `sched +0` runs after every prompt hook.
__clackatoo_prompt() {
    emulate -L zsh
    if [[ ${precmd_functions[-1]} != __clackatoo_prompt ]]; then
        precmd_functions=(${precmd_functions:#__clackatoo_prompt} __clackatoo_prompt)
        if zmodload zsh/sched 2>/dev/null; then
            sched +0 __clackatoo_mark_prompt
            return
        fi
    fi
    __clackatoo_mark_prompt
}

# The marks are in PS1 itself, so they come again when zsh draws the
# prompt again.
__clackatoo_mark_prompt() {
    emulate -L zsh
    if [[ $PS1 != *$'\e]133;A\a'* ]]; then
        PS1=$'%{\e]133;A\a%}'$PS1
    fi
    if [[ $PS1 != *$'\e]133;B\a'* ]]; then
        PS1=$PS1$'%{\e]133;B\a%}'
    fi
}

# Last of the hooks run before a command: its output starts here. The
# command goes with it, its bytes percent-encoded but for letters, digits
# and `/._~-`.
__clackatoo_preexec() {
    emulate -L zsh -o extended_glob
    local LC_ALL=C
    print -rnu $__clackatoo_fd -- \
        $'\e]133;C;cmdline_url='${1//(#m)[^A-Za-z0-9\/._~-]/%${(l:2::0:)$(( [##16] #MATCH ))}}$'\a'
}

# Adds the hooks. A .zshrc may set the hooks' lists afresh, so this waits
# until it has run: `sched +0` runs it before the first prompt, after the
# prompt hooks .zshrc added, so it does their work for that prompt
# itself.
__clackatoo_install() {
    emulate -L zsh
    typeset -ga precmd_functions preexec_functions
    precmd_functions=(
        __clackatoo_precmd
        ${precmd_functions:#__clackatoo_(precmd|prompt)}
        __clackatoo_prompt
    )
    preexec_functions=(${preexec_functions:#__clackatoo_preexec} __clackatoo_preexec)
    if (( ${1:-0} )); then
        __clackatoo_precmd
        __clackatoo_mark_prompt
    fi
}

if zmodload zsh/sched 2>/dev/null; then
    sched +0 __clackatoo_install 1
else
    __clackatoo_install
fi
