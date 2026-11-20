live_pid_running() {
    local state
    kill -0 "$1" 2>/dev/null || return 1
    state=$(ps -p "$1" -o stat= 2>/dev/null) || return 0
    [[ "$state" != *Z* ]]
}

prepare_live_workspace() {
    local root=$1 product=$2
    while [[ "$root" == ?*/ ]]; do root=${root%/}; done
    local marker="$root/.live-$product"
    if [[ -e "$root" || -L "$root" ]]; then
        if [[ -L "$root" || ! -O "$root" || ! -d "$marker" || -L "$marker" ]]; then
            printf 'live-up-%s: cannot reset %s. Check for an ongoing start or move the directory aside and retry.\n' "$product" "$root" >&2
            return 1
        fi
        rmdir -- "$marker" || return 1
        if [[ -f "$root/llmstub.pid" ]] && live_pid_running "$(cat "$root/llmstub.pid")"; then
            mkdir -- "$marker" || return 1
            printf 'live-up-%s: %s is in use. Run just live-down-%s.\n' "$product" "$root" "$product" >&2
            return 1
        fi
        rm -rf -- "$root" || return 1
    else
        mkdir -p -- "$(dirname -- "$root")" || return 1
    fi
    mkdir -m 700 -- "$root"
}

finish_live_start() {
    local status=$1 root=$2 product=$3
    if (( status != 0 )) && [[ -f "$root/llmstub.pid" ]]; then
        just --set "live_root_$product" "$root" "live-down-$product" || exit 1
    fi
    mkdir -- "$root/.live-$product" || status=1
    exit "$status"
}

start_live_check() {
    local root=$1 product=$2 interrupted=0 stop
    printf -v stop 'just --set %q %q %q' "live_root_$product" "$root" "live-down-$product"
    trap 'interrupted=130' INT
    trap 'interrupted=143' TERM
    just --set "live_root_$product" "$root" "live-up-$product" || return $?
    trap "$stop" EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    if (( interrupted )); then exit "$interrupted"; fi
}
