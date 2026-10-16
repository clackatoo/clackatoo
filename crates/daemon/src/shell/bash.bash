# clackatoo's shell integration for bash. clackatoo starts bash with this
# as its startup file, in place of ~/.bashrc, which it reads first.
#
# It marks where each prompt starts (OSC 133 A) and ends (B), where a
# command's output starts (C, with the command) and how the command
# exited (D), and says where the shell is (OSC 7), so clackatoo can tell
# commands apart, and knows when typing is read by the shell. See
# `clackatoo_term::blocks`.

# Before anything else runs, ~/.bashrc included: the secret clackatoo
# gave this terminal, which the C mark carries to say it's the shell's
# and not something a program printed. It's in a file only clackatoo and
# this user can read, named in the environment. It's read once and the
# file removed at once, and the name taken out of the environment, so no
# program bash runs from now on can learn it.
if [[ -n ${CLACKATOO_SECRET_FILE-} ]]; then
    IFS= read -r __clackatoo_secret 2>/dev/null <"$CLACKATOO_SECRET_FILE"
    command rm -f -- "$CLACKATOO_SECRET_FILE"
    unset CLACKATOO_SECRET_FILE
    [[ ${__clackatoo_secret-} =~ ^[0-9a-f]{32}$ ]] || unset __clackatoo_secret
fi

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

    # Last, after anything that sets PS1 afresh for each prompt. The marks
    # are in PS1 itself, so they come again when bash draws the prompt
    # again. Typing is only marked where commands are (PS0, bash 4.4 and
    # later): without C, there's no telling the prompt has been left.
    # The number the next line gets in the history is kept, to tell
    # whether it goes in: kept here, after prompt commands that read
    # other terminals' lines into the history (`history -c; history -r`).
    __clackatoo_prompt() {
        __clackatoo_history=$HISTCMD
        if [[ $PS1 != *'\e]133;A\a'* ]]; then
            PS1='\[\e]133;A\a\]'$PS1
        fi
        if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )) &&
            [[ $PS1 != *'\e]133;B\a'* ]]; then
            PS1=$PS1'\[\e]133;B\a\]'
        fi
    }

    # Where a command's output starts, with the secret and the command
    # line, its bytes percent-encoded but for letters, digits and
    # `/._~-`. Run in PS0, in a subshell, once a line has been read. The
    # line is the history's last only if that entry has the number the
    # prompt kept: a line bash kept out of the history (HISTCONTROL's
    # ignorespace or ignoredups, HISTIGNORE, or with history off) isn't
    # there, and isn't said, so it's never offered to run again.
    __clackatoo_command() {
        # Not traced, which would print the secret: this runs in a
        # subshell, so the shell itself is still traced.
        set +x
        local LC_ALL=C line= url= char i
        if [[ $HISTCMD != "${__clackatoo_history-}" ]]; then
            line=$(HISTTIMEFORMAT= builtin history 1)
            # `  NUMBER  LINE`, or `  NUMBER* LINE` for a changed entry.
            [[ $line =~ ^\ *([0-9]+)(\*\ |\ \ )(.*)$ &&
                ${BASH_REMATCH[1]} == "${__clackatoo_history-}" ]] &&
                line=${BASH_REMATCH[3]} || line=
        fi
        # A longer line isn't offered anyway.
        (( ${#line} <= 1024 )) || line=
        for ((i = 0; i < ${#line}; i++)); do
            char=${line:i:1}
            case $char in
                [A-Za-z0-9/._~-]) url+=$char ;;
                *) printf -v char '%%%02X' "'$char"; url+=$char ;;
            esac
        done
        printf '\e]133;C;clackatoo=%s%s\a' "$__clackatoo_secret" "${url:+;cmdline_url=$url}"
    }

    if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a'* ]]; then
        PROMPT_COMMAND=(__clackatoo_precmd "${PROMPT_COMMAND[@]}" __clackatoo_prompt)
    else
        # On lines of their own, which survive a trailing `;`.
        PROMPT_COMMAND=__clackatoo_precmd$'\n'${PROMPT_COMMAND:+$PROMPT_COMMAND$'\n'}__clackatoo_prompt
    fi
    # Shown after a command is read, before it runs (bash 4.4 and later).
    # The command and secret need PS0's `$(…)` expanded, which
    # `shopt -u promptvars` stops.
    if [[ -n ${__clackatoo_secret-} ]] && shopt -q promptvars; then
        PS0=$PS0'$(__clackatoo_command)'
    else
        PS0=$PS0'\e]133;C\a'
    fi
fi
