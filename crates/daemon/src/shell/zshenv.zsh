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
