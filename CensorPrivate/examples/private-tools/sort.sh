#!/bin/sh
set -eu
exec /usr/bin/jq -c '{order: (.inputs | to_entries | sort_by(.value.value, .key) | map(.key))}'
