# clackatoo's shell integration for bash. clackatoo starts bash with this
# as its startup file, in place of ~/.bashrc, which it reads first.
#
# It marks where each prompt starts (OSC 133 A), where a command's output
# starts (C) and how the command exited (D), and says where the shell is
# (OSC 7), so clackatoo can tell commands apart. See `clackatoo_term::blocks`.

if [[ -f ~/.bashrc ]]; then
    . ~/.bashrc
fi

if [[ $- == *i* && -z ${__clackatoo_integrated-} ]]; then
    __clackatoo_integrated=1

    # First of the prompt commands, while $? is still the command's status.
    __clackatoo_precmd() {
        local status=$?
        printf '\e]133;D;%s\a\e]7;file://%s%s\a' "$status" "$HOSTNAME" "${PWD//%/%25}"
        return "$status"
    }

    # Last, after anything that sets PS1 afresh for each prompt. The mark
    # is in PS1 itself, so it comes again when bash draws the prompt again.
    __clackatoo_prompt() {
        if [[ $PS1 != *'\e]133;A\a'* ]]; then
            PS1='\[\e]133;A\a\]'$PS1
        fi
    }

    if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a'* ]]; then
        PROMPT_COMMAND=(__clackatoo_precmd "${PROMPT_COMMAND[@]}" __clackatoo_prompt)
    else
        # On lines of their own, which survive a trailing `;`.
        PROMPT_COMMAND=__clackatoo_precmd$'\n'${PROMPT_COMMAND:+$PROMPT_COMMAND$'\n'}__clackatoo_prompt
    fi
    # Shown after a command is read, before it runs (bash 4.4 and later).
    PS0=$PS0'\e]133;C\a'
fi
