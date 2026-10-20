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

# The display variables clackatoo follows as the desktop logs in and out
# (see __clackatoo_display), and each as fish was started with it, before
# its startup files could set it, as /proc keeps that: `=VALUE`, or unset
# if it had none.
set -g __clackatoo_display_names WAYLAND_DISPLAY DISPLAY XAUTHORITY XDG_SESSION_TYPE XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP DESKTOP_SESSION SWAYSOCK I3SOCK HYPRLAND_INSTANCE_SIGNATURE NIRI_SOCKET
set -g __clackatoo_display_file (string replace -r '/[^/]*$' '' -- (status filename))/display
if test -r /proc/$fish_pid/environ
    while read -lz entry
        set -l pair (string split -m1 = -- $entry)
        if contains -- $pair[1] $__clackatoo_display_names
            set -g __clackatoo_display_$pair[1] "=$pair[2]"
        end
    end </proc/$fish_pid/environ
else
    for name in $__clackatoo_display_names
        if set -q $name
            set -g __clackatoo_display_$name "=$$name"
        end
    end
end

# The display the desktop's login gives the programs clackatoo starts,
# which the daemon writes to a small file whenever it changes: where it
# came from on a `#` line, then `NAME=VALUE` a line each, a name above
# that isn't there being unset. It's read before each prompt, by `read`
# alone, and only once it has changed are the variables set or erased,
# as text, never run. A variable is changed only while it's as fish
# started with it, or as this last made it: one you've set yourself is
# yours, and stays.
function __clackatoo_display --on-event fish_prompt
    test -f $__clackatoo_display_file; or return
    read -lz text <$__clackatoo_display_file
    if test -z "$text"; or test "$text" = "$__clackatoo_display_text"
        return
    end
    set -g __clackatoo_display_text $text
    set -l lines (string split \n -- $text)
    for name in $__clackatoo_display_names
        set -l kept __clackatoo_display_$name
        set -l want (string replace -rf -- "^$name=" = $lines)[1]
        set -l now
        if set -q $name
            set now "=$$name"
        end
        if test "$now" != "$$kept"; or test "$want" = "$$kept"
            continue
        end
        if test -n "$want"
            set -gx $name (string sub -s 2 -- $want)
            set -g $kept $want
        else
            set -e -g $name
            set -e -g $kept
        end
    end
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
