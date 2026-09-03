#!/bin/zsh
# Forum Agent launcher — double-click to start.
# Runs the server in a supervisor loop: if the app exits (operator clicked
# "Restart app" on the console, a crash, an update), it restarts in 2s.
# Close this terminal window to stop everything.
cd "$(dirname "$0")"
echo "Forum Agent starting — console: http://127.0.0.1:8710/control"
echo "Keep this window open. Close it to shut the Forum Agent down."
while true; do
  .venv/bin/python -m forum_agent.server
  echo "--- server exited; restarting in 2s (close this window to stop) ---"
  sleep 2
done
