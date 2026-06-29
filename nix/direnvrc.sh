# crossbridge — direnv integration helper.
#
# Source this from your direnv config (typically ~/.config/direnv/direnvrc, or
# whatever your dotfiles assemble that file from) to expose the `crossbridge_up`
# function. Then, in any crosslink repo's .envrc:
#
#     crossbridge_up <group> [slug]
#
# On directory entry it checks whether a crossbridge-server is already running
# for the repo; if not it starts one detached. Either way it exports
# CROSSBRIDGE_OWN_SLUG so every crossbridge-client invoked in that repo is
# pinned to the same slug the server registered with.
#
# Relies on the direnv runtime builtins `has`, `log_status`, and `log_error`,
# which are in scope whenever direnv evaluates a direnvrc / .envrc.

# Parse a repo slug from an origin remote URL, mirroring crossbridge-server's
# slug.rs::parse_slug so the slug we export always matches what the server
# would derive on its own.
_crossbridge_parse_slug() {
    local url=$1 path last
    # trim surrounding whitespace
    url=${url#"${url%%[![:space:]]*}"}
    url=${url%"${url##*[![:space:]]}"}
    [ -n "$url" ] || return 1
    case $url in
        *"://"*) path=${url#*://}; path=${path#*/} ;;  # scheme://[user@]host/<path>
        *:*)     path=${url#*:} ;;                      # scp-style git@host:<path>
        *)       path=$url ;;                           # bare local path
    esac
    path=${path%%[#?]*}   # drop ?query / #fragment
    path=${path%/}        # drop trailing slash
    last=${path##*/}      # last path segment
    last=${last%.git}     # drop .git suffix
    [ -n "$last" ] || return 1
    printf '%s' "$last"
}

# Read the origin remote URL of repo $1, preferring jj when a .jj/ dir is
# present — same source order as crossbridge-server.
_crossbridge_origin_url() {
    local root=$1 url
    if [ -d "$root/.jj" ]; then
        url=$(jj -R "$root" git remote list 2>/dev/null \
              | awk '$1 == "origin" { print $2; exit }')
        [ -n "$url" ] && { printf '%s' "$url"; return 0; }
    fi
    git -C "$root" remote get-url origin 2>/dev/null
}

# crossbridge_up <group> [slug]
#
# Ensure a crossbridge-server is running for the current repo, then export
# CROSSBRIDGE_OWN_SLUG so every crossbridge-client invoked here is pinned to
# the same slug the server registered with.
#
#   group  (required) coordination group to register under
#   slug   (optional) repo slug; derived from the origin remote when omitted
crossbridge_up() {
    local group=$1 slug=${2:-}

    if [ -z "$group" ]; then
        log_error "crossbridge_up: missing required <group> argument"
        return 1
    fi

    if ! has crossbridge-server; then
        log_error "crossbridge_up: crossbridge-server not on PATH; skipping"
        return 1
    fi

    # Canonical path — matches crossbridge-server's own canonicalize(--repo-path).
    local root
    root=$(pwd -P)

    # --- resolve slug -------------------------------------------------------
    if [ -z "$slug" ]; then
        local url
        url=$(_crossbridge_origin_url "$root")
        [ -n "$url" ] && slug=$(_crossbridge_parse_slug "$url")
        if [ -z "$slug" ]; then
            log_error "crossbridge_up: cannot derive slug for $root (no usable origin remote); pass it explicitly: crossbridge_up $group <slug>"
            return 1
        fi
    fi

    # The server refuses to start without an initialised crosslink DB.
    if [ ! -f "$root/.crosslink/issues.db" ]; then
        log_error "crossbridge_up: no .crosslink/issues.db in $root (run 'crosslink init' first); skipping"
        return 1
    fi

    # Export early so clients align even while the server is still starting
    # or backing off waiting for the supervisor.
    export CROSSBRIDGE_OWN_SLUG="$slug"

    local logdir=${XDG_STATE_HOME:-$HOME/.local/state}/crossbridge
    mkdir -p "$logdir"
    local logfile="$logdir/${slug}.${group}.log"

    # --- serialize check-and-start (flock) ----------------------------------
    # Two shells entering the repo at once would both pgrep, both see nothing,
    # and both spawn a server. flock makes check-then-start atomic: the second
    # contender blocks on the same lock until the first has started AND made its
    # server observable (the readiness poll below), then takes the lock and hits
    # the "already running" path. fd 200 is the conventional flock fd; we never
    # leak it to the detached server (200>&- on the spawn line). If flock is
    # missing we degrade to the original best-effort check — better than nothing.
    if has flock; then
        exec 200>"$logdir/${slug}.${group}.lock"
        # -w guards against a wedged holder; on timeout we proceed unlocked.
        flock -w 30 200 \
            || log_status "crossbridge: flock timed out; proceeding without start lock"
    fi

    # --- already running for THIS repo? -------------------------------------
    # A lone server (no same-group peers yet) creates no socket files, so the
    # only reliable key is the canonical --repo-path we always pass below.
    if pgrep -af crossbridge-server 2>/dev/null | grep -qF -- "--repo-path $root"; then
        log_status "crossbridge: server already running for '$slug' ($root)"
        exec 200>&-
        return 0
    fi

    # --- informational: is the supervisor up? ------------------------------
    local sroot=${CROSSBRIDGE_SOCKET_ROOT:-}
    [ -z "$sroot" ] && [ -n "${XDG_RUNTIME_DIR:-}" ] && sroot="$XDG_RUNTIME_DIR/crossbridge"
    [ -z "$sroot" ] && sroot=/run/crossbridge
    if [ ! -S "$sroot/register.socket" ]; then
        log_status "crossbridge: supervisor socket missing at $sroot/register.socket — server will back off and retry until it appears"
    fi

    # --- start detached -----------------------------------------------------
    # Detach fully: 0/1/2 go to the log, fd 3 is closed, and fd 200 (the start
    # lock) is closed so the long-lived server never inherits and pins it.
    # direnv evaluates .envrc with fd 3 wired to a pipe it reads until EOF. If
    # the server inherits that write end it is never closed, direnv never sees
    # EOF, and the shell that triggered .envrc hangs until the server exits.
    if has setsid; then
        setsid crossbridge-server \
            --group "$group" --slug "$slug" --repo-path "$root" \
            >>"$logfile" 2>&1 </dev/null 3>&- 200>&- &
    else
        nohup crossbridge-server \
            --group "$group" --slug "$slug" --repo-path "$root" \
            >>"$logfile" 2>&1 </dev/null 3>&- 200>&- &
    fi
    disown 2>/dev/null || true

    # --- readiness: hold the lock until the server is observable ------------
    # The spawn above forks immediately; the process only matches the pgrep key
    # once the child has exec'd the binary. Releasing the lock before then would
    # let the next contender re-detect "absent" and start a duplicate. Poll
    # (bounded) until it appears, so the lock we drop guards a visible server.
    for _ in $(seq 60); do
        pgrep -af crossbridge-server 2>/dev/null | grep -qF -- "--repo-path $root" && break
        sleep 0.05
    done

    exec 200>&-
    log_status "crossbridge: started server for '$slug' (group '$group'); logs: $logfile"
}
