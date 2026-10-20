#!/bin/sh
# ============================================================================
# TEST-ONLY STAND-IN. THIS IS NOT SSH.
# It performs no network access, no encryption, no authentication and no host
# key checking. It parses its arguments the way `ssh` lays them out and runs the
# "remote command" LOCALLY with `sh -c`, exactly as sshd would run it through the
# login shell. Tests put it on PATH (or pass it as the ssh program) to exercise
# our argument construction, the marker/stream handling and `rgit serve --stdio`
# end to end without a real sshd. It proves nothing about real SSH security.
#
# The behaviour is chosen by the destination host name, so tests can run in
# parallel without sharing environment variables:
#   mode-hostkey, mode-changed, mode-denied, mode-refused, mode-resolve,
#   mode-notfound, mode-silent, mode-echo, mode-dies  -> canned failures
#   anything else                                      -> run the command locally
# A host name starting with "noisy-" (e.g. noisy-mode-echo, noisy-srv) first prints a
# login banner to stdout, like a remote shell with chatty rc files.
# If remote.env exists next to this script it is sourced first: that file is the stand-in
# for the remote account's own environment.
# Every invocation appends its argv (one argument per line) to argv.log next to
# this script.
# ============================================================================
here=$(dirname "$0")
{
  echo "--- argv"
  for a in "$@"; do printf '%s\n' "$a"; done
} >> "$here/argv.log"

while [ $# -gt 0 ]; do
  case "$1" in
    -o|-p|-l|-F|-i|-J|-L|-R|-W|-b|-c|-D|-E|-e|-I|-m|-O|-Q|-S|-w) shift 2 ;;
    --) shift; break ;;
    -*) shift ;;
    *) break ;;
  esac
done
host="$1"
shift

case "$host" in
  noisy-*)
    echo "Welcome to FakeOS 1.0"
    echo "Last login: yesterday from nowhere"
    ;;
esac

case "${host#noisy-}" in
  mode-hostkey)
    echo "Host key verification failed." >&2; exit 255 ;;
  mode-changed)
    printf '@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n' >&2
    echo "Host key verification failed." >&2; exit 255 ;;
  mode-denied)
    echo "user@$host: Permission denied (publickey,password)." >&2; exit 255 ;;
  mode-refused)
    echo "ssh: connect to host $host port 22: Connection refused" >&2; exit 255 ;;
  mode-resolve)
    echo "ssh: Could not resolve hostname $host: nodename nor servname provided, or not known" >&2; exit 255 ;;
  mode-notfound)
    echo "sh: rgit: command not found" >&2; exit 127 ;;
  mode-silent)
    sleep 30; exit 0 ;;
  mode-echo)
    printf '\nRGIT-STDIO/1\n'; exec cat ;;
  mode-dies)
    printf '\nRGIT-STDIO/1\n'
    sleep 0.3
    echo "Connection to $host closed by remote host." >&2
    exit 255 ;;
esac

# the "remote machine" has its own environment (e.g. its own key directory)
[ -f "$here/remote.env" ] && . "$here/remote.env"
exec sh -c "$*"
