#!/bin/sh
set -eu

mkdir -p /app/.baihua/logs /app/.baihua/avatars /app/.baihua/files
if ! chown baihua:baihua /app/.baihua /app/.baihua/logs /app/.baihua/avatars /app/.baihua/files; then
    echo "Warning: could not change ownership of one or more application directories." >&2
fi
if ! gosu baihua:baihua sh -c 'for directory in /app/.baihua /app/.baihua/logs /app/.baihua/avatars /app/.baihua/files; do test -w "$directory" && test -x "$directory" || exit 1; done'; then
    echo "Error: the baihua user cannot write to the application directories." >&2
    exit 1
fi
exec gosu baihua:baihua "$@"
