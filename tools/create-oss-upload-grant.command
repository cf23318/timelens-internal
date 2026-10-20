#!/bin/sh
set -eu
task_tool_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
export PYTHONPATH="$task_tool_dir/../upload-server${PYTHONPATH:+:$PYTHONPATH}"
exec python3 -m timelens_upload.oss_grant "$@"
