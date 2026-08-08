# The nondeterministic-field mask (PORTING.md §6, Phase 0 deliverable).
#
# Every rule here names a field that legitimately differs between two runs of
# the SAME implementation. Without them a read-only diff cries wolf on every
# clock tick and the harness becomes noise you learn to ignore.
#
# A rule earns its place by showing up in `parity mask` — do not add one
# speculatively, and never widen one to paper over a real difference.
#
# Run-scoped paths are substituted by bin/parity before this file is applied.

# Wall-clock stamps: '2026-08-08 20:41:07' and the audit log's '+0800' variant.
s/[0-9]\{4\}-[0-9][0-9]-[0-9][0-9] [0-9][0-9]:[0-9][0-9]:[0-9][0-9] [+-][0-9]\{4\}/<TS>/g
s/[0-9]\{4\}-[0-9][0-9]-[0-9][0-9] [0-9][0-9]:[0-9][0-9]:[0-9][0-9]/<TS>/g

# Process identity: pids move every run, and the audit context embeds them.
s/pid=[0-9][0-9]*/pid=<PID>/g
s/ppid=[0-9][0-9]*/ppid=<PID>/g
s/(pid [0-9][0-9]*)/(pid <PID>)/g

# Elapsed time reported by an operation ('rc=0 (3s)').
s/rc=\([0-9-]*\) ([0-9][0-9]*s)/rc=\1 (<DUR>s)/g

# `rowt report` names its saved file after the wall clock, so the FILENAME
# varies between runs and not just the contents — this rule has to apply to the
# fsstate listing too, which is why it matches the bare name anywhere.
s/diag-[0-9]\{8\}-[0-9]\{6\}\.txt/diag-<TS>.txt/g

# Temp files created per run. Only the volatile PREFIX is masked — matching
# through to the end of the token would swallow the rest of the path too, and
# a file written to the wrong place inside the sandbox would then be invisible.
s#/tmp/[A-Za-z0-9_.-]*\.[A-Za-z0-9]\{6,\}#<TMP>#g
s#/var/folders/[^ ]*/T/*[A-Za-z0-9._-]*#<TMP>#g
