#!/bin/sh
for candidate in \
    "$(command -v br8n)" \
    "${BR8N_DB:+${BR8N_DB%/*}/bin/br8n}" \
    "${XDG_DATA_HOME:-$HOME/.local/share}/br8n/bin/br8n" \
    "$HOME/Library/Application Support/br8n/bin/br8n"
do
    if [ -f "$candidate" ] && [ -x "$candidate" ]; then
        exec "$candidate" "$@"
    fi
done

missing="br8n is not installed, so the br8n plugin cannot search your notes. Install it with the one-line installer at https://github.com/Jagma/br8n#install, then restart Claude Code."
case "$1 $2" in
"hook session-start")
    printf '{"systemMessage": "%s", "hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": "%s"}}\n' "$missing" "$missing"
    ;;
"hook "*)
    ;;
*)
    echo "$missing" >&2
    exit 1
    ;;
esac
