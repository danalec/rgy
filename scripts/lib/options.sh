#!/usr/bin/env bash
#
# The option vocabulary every script in this directory shares, and the twin
# of scripts/lib/options.ps1.
#
# A script in this directory is a twin of a script there: the same output,
# the same exit code, the same answer to the same input. That includes the
# answer to an input neither of them understands. An option the script does
# not know is an error -- the same words and the same exit code on both
# platforms.
#
# The reason is not tidiness. A script that ignores what it does not
# understand does the real thing when the caller asked for a dry run, and
# in this directory the real thing is a release: a tag, an archive, a
# release page on the forgejo and on GitHub, an installer that points at
# it. The mistake is silent, and the release is out before anybody reads
# the log.
#
# The vocabulary:
#
#   --dry-run      -DryRun       one option, two spellings: the long one
#                               is the twin's, the capitalised one is
#                               PowerShell's, and a caller who writes
#                               either gets the same answer on either
#                               platform.
#   --version X    -Version X    an option with a value. The value is the
#                               next argument, or the part after the
#                               colon (-Version:X).
#   --queries X    -Queries X    an option that may be given more than
#                               once. A variable named with [] collects
#                               every occurrence, so --queries a --queries
#                               b leaves the script with a list of two.
#
# The name is matched with the dashes and the case off, so --dry-run,
# -DryRun and --dryrun are one option.
#
# The caller declares what it takes and names its variables: the switches,
# the values, and the slots a positional argument goes into.
#
#   rgy_read_options "skiptests=skip_tests dryrun=dry_run" \
#                    "version=version" "ref=ref" "$@"
#
# and the variables are set in the caller's scope. Bash has no way to say
# that in a function, but eval does: an assignment inside a function that
# has no local of that name is a global one.

# rgy_help_requested "$@" -- does the caller want the help? The names are
# matched the way the option names are, so -h, --help and -Help are one
# question, and the answer does not depend on where the token sits on the
# command line.
rgy_help_requested() {
    local _arg _key
    for _arg in "$@"; do
        _key=$(printf '%s' "$_arg" | tr 'A-Z' 'a-z')
        _key="${_key//-/}"
        _key="${_key//_/}"
        case "$_key" in
            h|help) return 0 ;;
        esac
    done
    return 1
}

# rgy_option_lookup <table> <key> -- prints the variable name the table
# gives for the key, and fails when the table does not carry it.
rgy_option_lookup() {
    local _entry _canonical
    for _entry in $1; do
        _canonical="${_entry%%=*}"
        if [ "$_canonical" = "$2" ]; then
            printf '%s' "${_entry#*=}"
            return 0
        fi
    done
    return 1
}

# rgy_option_nth <table> <index> -- the variable name the index'th slot of
# the table gives, and fails when the table carries no such slot.
rgy_option_nth() {
    local _entry _index=0
    for _entry in $1; do
        if [ "$_index" -eq "$2" ]; then
            printf '%s' "${_entry#*=}"
            return 0
        fi
        _index=$((_index + 1))
    done
    return 1
}

# rgy_read_options <switches> <values> <positionals> "$@"
#
#   switches:    "canonical=variable canonical=variable"
#   values:      "canonical=variable canonical=variable"
#   positionals: "canonical=variable canonical=variable", the slots a
#                positional argument goes into, in the order they are
#                written. A script that takes none says so with an empty
#                table, and then every argument that is not an option is an
#                error.
#
# The rest are the arguments of the calling script. An option that is in
# neither table is an error, and so is a value with nothing after it.
#
# A switch is set to 1 -- the shell's "on" -- and a value is set to the
# text of the argument. The scripts here test their switches with
# `[ "$dry_run" -eq 1 ]`, and a helper that set them to "true" would
# break every one of those tests.
rgy_read_options() {
    local _switches="$1"
    local _values="$2"
    local _positionals="$3"
    shift 3
    local _written _name _value _have _key _var _slot=0
    while [ $# -gt 0 ]; do
        _written="$1"
        _name="$_written"
        _value=""
        _have=0
        # -Name:value carries the value with it.
        case "$_written" in
            *:*)
                _name="${_written%%:*}"
                _value="${_written#*:}"
                _have=1
                ;;
        esac
        # The name with the dashes and the case off: --dry-run, -DryRun and
        # --dryrun are one option. The dashes go with the shell's own
        # expansion and not with tr: `tr -d '-_'` reads the dash as the
        # start of an option, and the name comes out empty.
        _key=$(printf '%s' "$_name" | tr 'A-Z' 'a-z')
        _key="${_key//-/}"
        _key="${_key//_/}"
        if _var=$(rgy_option_lookup "$_switches" "$_key"); then
            if [ "$_have" -eq 1 ]; then
                # A switch takes no value. -DryRun:$false is a name the
                # script does not know, and saying so is better than
                # guessing what the caller meant.
                echo "unknown option: $_written" >&2
                exit 2
            fi
            eval "$_var=1"
            shift
        elif _var=$(rgy_option_lookup "$_values" "$_key"); then
            if [ "$_have" -eq 0 ]; then
                if [ $# -lt 2 ]; then
                    echo "option $_written needs a value" >&2
                    exit 2
                fi
                _value="$2"
                shift
            fi
            # A variable named with [] takes every occurrence: an option that
            # may be given more than once collects its values instead of
            # keeping the last one.
            case "$_var" in
                *'[]')
                    _var="${_var%'[]'}"
                    eval "$_var+=(\"\$_value\")"
                    ;;
                *)
                    eval "$_var=\$_value"
                    ;;
            esac
            shift
        else
            # Not an option. It is a positional when it does not look like
            # one, and the script declares the slot it goes into; a
            # positional is the argument as it was written, colon and all --
            # a Windows path is not an option with a value.
            case "$_written" in
                -*) _var="" ;;
                # The || keeps the script alive when the table carries no
                # such slot: the lookup fails, and a failed substitution in
                # an assignment is a failed command under `set -e`.
                *) _var=$(rgy_option_nth "$_positionals" "$_slot") || _var="" ;;
            esac
            if [ -z "$_var" ]; then
                echo "unknown option: $_written" >&2
                exit 2
            fi
            eval "$_var=\$_written"
            _slot=$((_slot + 1))
            shift
        fi
    done
}
