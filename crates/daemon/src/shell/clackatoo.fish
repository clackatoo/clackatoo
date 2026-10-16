# clackatoo's shell integration for fish, which clackatoo starts with
# `-C 'source THIS'`, after fish has read its own startup files.
#
# fish 4 marks prompts and commands itself (OSC 133), with each command
# in its C mark. Any program can print a mark like that, so this marks
# each command's start again, with the secret clackatoo gave this
# terminal, which no program fish runs is told. Only a command marked
# with it is offered to run again after a restart. See
# `clackatoo_term::blocks`.

if set -q __clackatoo_integrated
    exit 0
end
set -g __clackatoo_integrated 1

# The secret is in a file only clackatoo and this user can read, named
# in the environment. It's read once and the file removed at once, and
# the name taken out of the environment, so no program fish runs from
# now on can learn it.
if set -q CLACKATOO_SECRET_FILE
    if test -f "$CLACKATOO_SECRET_FILE"
        read -l secret <$CLACKATOO_SECRET_FILE
        if string match -qr '^[0-9a-f]{32}$' -- $secret
            set -g __clackatoo_secret $secret
        end
    end
    command rm -f -- $CLACKATOO_SECRET_FILE 2>/dev/null
    set -e CLACKATOO_SECRET_FILE
end

# fish before 4 marks nothing itself, and a C alone would start a command
# that never ends.
set -l major (string split -m1 . -- $version)[1]
if set -q __clackatoo_secret; and test "$major" -ge 4 2>/dev/null
    # Run before each command, after fish's own C mark: the command, its
    # bytes percent-encoded but for letters, digits and `/._~-`. A
    # command fish keeps out of its history, as a private fish
    # (`fish --private`) does every one, or one `fish_should_add_to_history`
    # (fish 4.0 and later) turns away, is marked without the command, so
    # it's never offered to run again.
    function __clackatoo_preexec --on-event fish_preexec
        set -l mark "clackatoo=$__clackatoo_secret"
        if not set -q fish_private_mode
            and begin
                not functions -q fish_should_add_to_history
                or fish_should_add_to_history $argv[1] >/dev/null 2>&1
            end
            set mark "$mark;cmdline_url="(string escape --style=url -- $argv[1])
        end
        printf '\e]133;C;%s\a' $mark
    end
end
