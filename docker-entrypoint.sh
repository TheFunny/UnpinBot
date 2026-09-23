#!/bin/sh
set -e
if [ "$(id -u)" = "0" ]; then
    UID_="${LOCAL_USER_ID:-9001}"
    # A root or malformed LOCAL_USER_ID would silently defeat (or crash
    # confusingly) the privilege drop; fail here, naming the variable, while
    # the operator still watches the logs. `su-exec` takes a plain numeric
    # uid:gid without any passwd entry, so no user account is created.
    case "$UID_" in
        ''|*[!0-9]*)
            echo "entrypoint: LOCAL_USER_ID must be numeric, got '$UID_'" >&2
            exit 1
            ;;
    esac
    if [ "$UID_" -eq 0 ]; then
        echo "entrypoint: LOCAL_USER_ID must be non-zero" >&2
        exit 1
    fi

    # The state volume must be writable by the runtime user. Ensure the
    # directory exists first (the mount point exists by compose definition),
    # then chown it. Bind mounts on restrictive filesystems may refuse chown
    # — surface that loudly instead of failing later with a cryptic EACCES
    # from the bot itself.
    mkdir -p /app/pers_data 2>/dev/null || true
    # chown BOTH owner and group: `chown 1000 dir` leaves the group as-is
    # (typically root/0 on bind mounts).
    if ! chown -R "$UID_:$UID_" /app/pers_data 2>/dev/null; then
        echo "entrypoint: WARNING: chown /app/pers_data failed;" >&2
        echo "  the bot may be unable to persist state. Fix on the host:" >&2
        echo "  sudo chown -R $UID_:$UID_ \$(pwd)/pers_data" >&2
    fi

    exec su-exec "$UID_:$UID_" "$@"
fi
exec "$@"
