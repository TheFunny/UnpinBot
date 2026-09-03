#!/bin/sh
set -e
if [ "$(id -u)" = "0" ]; then
    UID_="${LOCAL_USER_ID:-9001}"
    # `docker compose restart` / `docker restart` reuse the same container, so
    # the passwd entry from the first boot persists: a second `adduser` exits 9
    # and `set -e` would kill the container on every restart. Create only if
    # missing; realign the UID otherwise so LOCAL_USER_ID changes still apply.
    if id unpin > /dev/null 2>&1; then
        # busybox adduser cannot modify; recreate is only safe via deluser on
        # alpine: keep it simple by re-aligning through usermod-less approach.
        if [ "$(id -u unpin)" != "$UID_" ]; then
            deluser unpin 2>/dev/null || true
            adduser -D -H -u "$UID_" -s /sbin/nologin unpin 2>/dev/null || true
        fi
    else
        adduser -D -H -u "$UID_" -s /sbin/nologin unpin 2>/dev/null || true
    fi

    # The state volume must be writable by the runtime user. Ensure the
    # directory exists first (the mount point exists by compose definition),
    # then chown it. Bind mounts on restrictive filesystems may refuse chown
    # — surface that loudly instead of failing later with a cryptic EACCES
    # from the bot itself.
    mkdir -p /app/pers_data 2>/dev/null || true
    if ! chown -R "$UID_" /app/pers_data 2>/dev/null; then
        echo "entrypoint: WARNING: chown /app/pers_data failed;" >&2
        echo "  the bot may be unable to persist state. Fix on the host:" >&2
        echo "  sudo chown -R $UID_ \$(pwd)/pers_data" >&2
    fi

    exec su-exec "$UID_" "$@"
fi
exec "$@"
