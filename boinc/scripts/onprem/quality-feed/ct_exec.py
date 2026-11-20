"""Run a command in the batch container, or directly when already inside it.

A container id is a positive integer, reached with `pct exec <id> -- ...`, or the literal
`local`, which runs the command itself. The BOINC server container is not routed through this
module: its commands keep their explicit `ssh <host> pct exec <id> -- ...` form.
"""
import argparse
import os
import subprocess

LOCAL = "local"


def ctid_arg(value):
    """argparse type: a positive integer container id, or `local`. Returns an int or `local`."""
    text = str(value)
    if text == LOCAL:
        return LOCAL
    if text.isascii() and text.isdigit() and int(text) >= 1:
        return int(text)
    raise argparse.ArgumentTypeError("expected a positive container id or 'local'")


def default_ctid():
    """The CTID environment variable when set, else None; the caller then requires --ctid."""
    value = os.environ.get("CTID")
    if value:
        try:
            return ctid_arg(value)
        except argparse.ArgumentTypeError:
            raise SystemExit("CTID must be a positive container id or 'local'") from None
    return None


def ct_argv(ctid, argv):
    """The argument list that runs `argv` in container `ctid`, or as-is for `local`."""
    argv = list(argv)
    if str(ctid) == LOCAL:
        return argv
    return ["pct", "exec", str(ctid), "--", *argv]


def ct_run(ctid, argv, **kwargs):
    return subprocess.run(ct_argv(ctid, argv), **kwargs)
