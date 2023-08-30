#!/bin/bash

set -e
if [ "$(id -u)" -eq '0' ]
then
   USER_ID=${LOCAL_USER_ID:-9001}

   useradd --shell /bin/bash -u ${USER_ID} -o -c "" -m user > /dev/null 2>&1
   usermod -a -G root user > /dev/null 2>&1
   chown -R `id -u user`:`id -u user` /app > /dev/null 2>&1

   export HOME=/home/user
   exec gosu user "$0" "$@"
fi

exec "$@"
