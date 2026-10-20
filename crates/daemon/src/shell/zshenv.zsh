# clackatoo's shell integration for zsh, part one. clackatoo starts zsh
# with ZDOTDIR pointing at the directory this is in, so zsh reads this in
# place of the user's own .zshenv.
#
# It puts ZDOTDIR back as it was, reads the user's .zshenv, and in an
# interactive shell loads the integration itself (clackatoo.zsh). zsh then
# reads the user's .zprofile, .zshrc and .zlogin from ZDOTDIR as it always
# would, wherever their .zshenv left it, so their config runs as usual,
# and a ZDOTDIR they set themselves is kept.
#
# Commands are quoted so that aliases can't change them.

'builtin' 'typeset' -g __clackatoo_zsh=$ZDOTDIR/clackatoo.zsh

# The display variables clackatoo follows as the desktop logs in and out
# (see __clackatoo_display in clackatoo.zsh), and each as this shell was
# started with it, before the user's own startup files could set it:
# `=VALUE`, or unset if it had none.
if [[ -o interactive ]]; then
    'builtin' 'typeset' -ga __clackatoo_display_names
    __clackatoo_display_names=(WAYLAND_DISPLAY DISPLAY XAUTHORITY XDG_SESSION_TYPE XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP DESKTOP_SESSION SWAYSOCK I3SOCK HYPRLAND_INSTANCE_SIGNATURE NIRI_SOCKET)
    'builtin' 'typeset' -g __clackatoo_display_file=${ZDOTDIR%/*}/display
    () {
        'builtin' 'emulate' -L zsh
        'builtin' 'local' name
        for name in $__clackatoo_display_names; do
            if (( ${(P)+name} )); then
                'builtin' 'typeset' -g "__clackatoo_display_$name==${(P)name}"
            fi
        done
    }
fi

# Before anything else runs, the user's .zshenv included: the secret
# clackatoo gave this terminal, which the C mark carries to say it's the
# shell's and not something a program printed. It's in a file only
# clackatoo and this user can read, named in the environment. It's read
# once and the file removed at once, and the name taken out of the
# environment, so no program zsh runs from now on can learn it. Never
# traced, which would print it.
if [[ -n ${CLACKATOO_SECRET_FILE-} ]]; then
    () {
        'builtin' 'setopt' local_options no_xtrace
        'builtin' 'typeset' -g __clackatoo_secret=
        if [[ -r $CLACKATOO_SECRET_FILE ]]; then
            IFS= 'builtin' 'read' -r __clackatoo_secret <$CLACKATOO_SECRET_FILE
        fi
        'command' 'rm' -f -- $CLACKATOO_SECRET_FILE 2>/dev/null
        'builtin' 'unset' CLACKATOO_SECRET_FILE
        if [[ ! -o interactive || ${#__clackatoo_secret} != 32 ||
            $__clackatoo_secret == *[^0-9a-f]* ]]; then
            'builtin' 'unset' __clackatoo_secret
        fi
    }
fi
if [[ -n ${CLACKATOO_ZSH_ZDOTDIR+set} ]]; then
    ZDOTDIR=$CLACKATOO_ZSH_ZDOTDIR
    'builtin' 'unset' CLACKATOO_ZSH_ZDOTDIR
else
    'builtin' 'unset' ZDOTDIR
fi

if [[ -f ${ZDOTDIR:-$HOME}/.zshenv ]]; then
    'builtin' 'source' ${ZDOTDIR:-$HOME}/.zshenv
fi

if [[ -o interactive && -f $__clackatoo_zsh ]]; then
    'builtin' 'source' $__clackatoo_zsh
fi
'builtin' 'unset' __clackatoo_zsh
